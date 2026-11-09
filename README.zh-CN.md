# phasegent

[English](README.md)

`phasegent` 是面向 OpenCode provider 工作流的角色化 CLI，用一套命令行接口
处理 issue、结构化 agent record、评论和工作流自动化。

## 安装

要求：Rust stable 和 Cargo。

```sh
git clone https://forgejo.cloud1ful.com/tools/phasegent.git
cd phasegent
cargo install --path . --force --features postgres,notify-dingtalk,notify-email
```

checkout 构建（`cargo install --path .`）激活本地源码；release 包激活对应 tag
二进制；`phasegent plugin install` 激活 OpenCode worktree 适配器；`phasegent skill
install` 部署单一 `phasegent` skill。一次验证不要混用。

Windows x64 用户可安装对应版本的
`phasegent-<tag>-x86_64-pc-windows-msvc.msi` release 文件。它只为当前用户安装到
`%LOCALAPPDATA%\Programs\phasegent-cli`，并将该目录加入用户 PATH；打开新终端后即可使用
`phasegent`。可在 Windows 设置 > 应用 > 已安装的应用中卸载。用户配置与 credential
会保留。由于 MSI 未签名，Windows 可能显示安全警告。

## 快速开始

为需要使用 CLI 的每个 role 配置 credential。credential 通过安全提示或 stdin
读取，不接受命令行明文参数。

```sh
# 安全提示输入
phasegent admin auth setup

# 从受保护文件或其他安全来源读取
phasegent admin auth setup --stdin < /secure/path/token

# 显式选择其他 provider 及其 API base
phasegent --provider redmine admin auth setup \
  --stdin --api-base https://redmine.example.com

# 将 credential 写入某个 agent role 而不是默认的 Admin 行
phasegent admin auth setup --provider redmine --stdin --api-base https://redmine.example.com --target-role executor < /secure/path/executor-key

# 准备 Redmine 的 project 和 role membership
phasegent --provider redmine admin workflow bootstrap \
  --repository OWNER/REPOSITORY
```

Redmine REST 地址是机器级的：`auth setup --api-base` 与
`admin config set redmine-api-base <URL>` 保存所有 role 共用的同一个地址，
`config show` 只在 `global_settings` 中报告一次。credential、已开通的身份、
provider 选择以及 Redmine close-status id 仍是 role 级。

`workflow bootstrap` 为每个 agent role 开通一个 Redmine 服务账号——
orchestrator、executor、reviewer 和 explore——通过 admin API 读取各自的
API key，把 key 存入 SQLite，并协调它们的直接 project membership
（Maintainer、Developer、Reporter、Reporter）。该命令是幂等的：重复执行会
复用已持有的全部身份，只创建缺失的账号。

`admin workflow bootstrap --all-projects` 会在全部现有 Redmine project 上
（而不是当前仓库对应的 project）协调这四个账号。它无需 Git 仓库，在任何
写入之前枚举完整 project 集合，只一次性开通共享身份，并仅补齐缺失的
membership，同时保留已附加的其他 role。加上 `--dry-run` 得到只读预览：
规划相同的目标结果，但不创建账号、不通过 API 获取用户 API key、不写入
membership 或 settings。两种模式都会打印完整 JSON report，并在所有 project 就绪时退出
`0`，任一 project blocked、warning 或 error 时退出 `1`。两种模式仍可能打开
本地 SQLite store，当 schema 或数据库文件缺失时会创建它。

可选原生 Git hooks：在仓库中运行 `phasegent hooks install`。消息处理、固定注释标记及安全更新/恢复说明见 [Git hooks](docs/git-hooks.md)。

## 常用命令

```sh
phasegent issue search --query "bug"
phasegent issue get 123
phasegent issue get 123 124 125
phasegent comment list 123
phasegent record list 123
phasegent record get 123 42
phasegent issue create \
  --title "Short title" --body "Issue details"
phasegent issue update 123 --body "Updated details"
phasegent issue close 123
phasegent issue status
phasegent issue branches 123
phasegent doctor
```

分支关联是本地只读查询：`issue status` 显示当前分支及其关联 issue 与缓存状态，`issue branches N` 列出本仓库中关联到该 issue 的全部分支（跨全部 scope）。

## 输出

每个命令都把结果写到 stdout，把诊断写到 stderr；格式由 `--output
auto|text|json` 选择（默认 `auto`）：`auto` 在交互式人类终端使用人类可读文本，
在其他情况（管道、CI、任意 agent role）使用稳定的 JSON 文档。`--no-progress`
只关闭 stderr 进度动画，不改变 warning 与 error。`--help` 与 `--version`
始终输出文本。

```sh
phasegent issue get 123                    # 终端为文本，管道为 JSON
phasegent --output text issue get 123      # 强制人类可读视图
phasegent --output json issue get 123      # 强制机器文档
```

支持的 collection list 默认在文本与 JSON 中返回第 1 页、最多 50 项。JSON
使用统一分页 envelope；使用 `--page` 和 `--limit` 选择其他页面，例如
`phasegent worktree list --page 2 --limit 50`。OpenCode adapter 需要完整 lease collection
时会读取全部页面。已文档化命令仍支持旧的 `--format`/`--json` 参数。
详见[输出模式](docs/cli-output.md)、[分页](docs/cli-pagination.md)和
[worktree 维护](docs/worktree-maintenance.md)。

选择的 provider 不是默认值时，在命令上添加 `--provider redmine` 或
`--provider local`。可以使用 `--project-id ID` 覆盖 project 的自动发现；
`--repository OWNER/REPOSITORY` 指定 Git 主机上的仓库，用于
`workflow bootstrap` 与 project 发现。

Forgejo 与其他 provider 一样通过统一配置选择：创建 issue、用 `issue bind` 接管已有编号，
或操作任何没有持久 task 的编号时，使用
`--provider forgejo --repository owner/repo --api-base INSTANCE_ROOT`，并通过
`admin auth setup --provider forgejo --repository owner/repo --api-base INSTANCE_ROOT --target-role <role>`
保存该 role 的 token（省略 `--target-role` 则写入 Admin 行）。通过环境变量、TOML、已保存的机器级默认值或 role 配置设定的 Forgejo
默认值会走同一套选择优先级（最终回退到 Redmine），因此无需显式 `--provider`；仓库依次由显式
`--repository`、当前 checkout 中能证明受信实例的 origin、Git 之外的已保存默认值确定。
`INSTANCE_ROOT` 是不带 `/api/v1` 后缀的受信实例地址，客户端会自行追加该前缀；
显式 `--api-base` 必须与其规范化后完全一致。
已带持久 Forgejo task 的编号会自动路由到该 task 保存的 instance
与 repository，无需重复这些参数；不同的显式 repository 不会重新绑定该 task。只需操作者准备 token。
Forgejo 使用仓库内 issue 编号，并在结果中保留原生全局 id；它支持 issue、评论、record、仓库读取、
milestone（作为 version）、原生 `Open` 与 `Closed` 状态，以及按 `owner/repo` 隔离的分支关联、worktree
lease、清理与 sync。不支持时间记录、relation、附件、子任务/planning 与 project
创建。Forgejo issue 没有状态标签：关闭之前一直是 `Open`，阶段细节记录在 record 中，
close 清理仅限匹配的 `owner/repo` scope；
Redmine issue 没有自动 PR 关联。详见 [Forgejo provider](docs/forgejo-provider.md)。

普通人类 CLI 不需要 agent role：没有经过验证的 host broker 上下文、也没有受管标记时，
即以 Operator 运行并集中使用 admin credential。agent role 由本地 host broker 验证，
绝不由环境变量选择；没有经过验证的 role 的受管会话会在任何 credential 或网络访问之前
fail-closed。`PHASEGENT_ROLE` 只是 legacy 标记，不授予任何身份。为特定 agent role
提供 credential 属于人类写操作：`admin auth setup --target-role <role>` 只选择存储行，
不改变调用者身份。

已授权的 `issue create` 可能以内建系统准备步骤触发 auto-bootstrap（admin
credential；单匹配或显式 `--project-id` 跳过，多匹配在写入前失败；无 fallback，
不授予调用者 admin 权限）。Operator 不能伪造 agent record：`record create`
被拒绝，请改用 `comment create`。详见 [docs/cli-caller-context.md](docs/cli-caller-context.md)。

Provisioning（`auth setup`、config 写操作、`workflow bootstrap`）位于
人类操作者专用的 `admin` 组（`phasegent admin ...`），AI role 永不调用。

完整命令参考见 `phasegent --help`（或 `phasegent --help <topic>`）。
`phasegent skill install` 将单一 `phasegent` skill 部署到 agent home skills 根
（默认 `~/.agents/skills`；`--project` 用于当前 checkout；`--path DIR` 指定根），
host 的文件系统 skill 发现即可加载。它选择 tracking 模式（`INLINE` /
`TRACKED_ISSUE` / `LOCAL_ISSUE`），通过 `--provider` 从配置解析 provider
（最终回退 Redmine），并记录当前的 `issue update`、结构化 `record` 发布、
`worktree acquire --base`、`worktree probe` 与 `worktree prune` 用法。

## Record

`record create` / `record get` / `record list` 是结构化笔记的官方路径：CLI
生成唯一的带版本元数据 header，agent 只提供元数据与纯文本正文，子会话绝不手写
header。record 的 id 即其原生引用（Redmine 为 `#change-<id>`，本地为
`#note-<id>`），`record get ISSUE RECORD_ID` 会读回纯文本正文与结构化字段，
因此父会话引用 record id 即可，无需转录笔记。kind 绑定会话 role：`executor` 与
`reviewer` 记录保留各自的 status/verdict 语义，只读的 `explore` role 只能发布
经授权的 recon record。

tracking 仅支持 Redmine 或显式的离线 local；不受支持的 provider 名称会在任何网络
调用前以可操作的 config 错误被拒绝。引用协议、稳定请求键与 Redmine/local 边界
详见 [docs/agent-records.md](docs/agent-records.md)。

## Worktree

lease 以 `(repo, issue, session)` 为键。`phasegent worktree acquire --issue N
[--session S] [--base REF]` 仅限 orchestrator：同一组合优先返回既有 lease；
显式 `--base REF` 会从该 ref（而非 `HEAD`）新建 worktree 和
`phasegent/<issue>-<short6hex>` 分支，且不复用当前 checkout。无法解析的 ref
会在写入任何 worktree、分支或 lease 之前本地失败。

`phasegent worktree probe [--path PATH | --issue N [--session S]]` 以有界 JSON
报告 checkout 状态：路径是否存在、是否为 Git worktree、clean/dirty/unknown、
分支与 `HEAD`、是否主 checkout，以及匹配的 lease。`--path` 与 `--issue` 互斥，
`--session` 用于收窄 `--issue`，两者都不传则探测当前 checkout。它是只读命令
（orchestrator、executor、reviewer）：不调用 provider、不写 lease、不同步、不
删除、不修复；`--issue` 无匹配 lease 时返回稳定的空结果，绝不猜测路径。

```sh
phasegent worktree acquire --issue 123 --base main
phasegent worktree probe --issue 123
```

由 OpenCode 会话（`ses_…`）租用的目录会被保护，直到 host 确认该会话已不在其中：
`issue close` 会先读取该目录的占用情况与 host 上报的会话谱系，把该目录 lease 家族中的空闲成员——
lease 指名的会话以及由它们派生的子会话——通过 OpenCode API（`opencode api session.move`）送回
仓库主 checkout，再移除目录；无法确认时保留目录，待 `issue sync` 确认会话已转移后
再移除。清理完成后，成功的 close 还会在已核实的主 checkout 干净且安全时恢复
issue 创建前的分支，并只对配置给该分支的上游执行 fast-forward。任何缺失、不安全或
冲突状态都会保留 checkout 原状并给出 warning；重复执行 `issue close` 会重试未完成的
上游更新，成功更新后或分支没有配置上游时则为 no-op。
`phasegent worktree prune` 是全局、默认执行移除的清理：不传 `--repo` 时清理本地
lease 存储记录的所有 repository identity，`--dryrun`（别名 `--dry-run`）为只读
预览。只有当目录关联的每个 issue 都在配置的 provider 上确认已关闭、没有 `active`
lease 引用该目录、且工作树 Git 干净时才会移除——没有等待期，`--stale-days` 只影响
显式的 `--release-stale` 恢复。它复用与 close/sync 相同的本地保护——绝不删除主
checkout、绝不动有 `active` lease 的路径、绝不删除 dirty/untracked 内容，唯一例外是
经过核实的根 `.cache` 目录：`.cache` 是真实根目录（不是文件、符号链接、`.cache2` 或
嵌套的 `other/.cache`）、其下没有被 Git 跟踪的路径、porcelain 状态只列出该根下的 `??`
路径，且清单记录可靠的 device/inode 与高分辨率变更证据，初始资格基线跨会话迁移保留，
删除前立即重读的清单必须与之一致；无法提供可靠身份的平台上该例外直接 fail closed；随后执行
普通的不带 `--force` 的 `git worktree remove`——并新增
自己的运行中会话保护：只有连接的共享前台 drain 服务报告为*当前运行*、且位于该目录
的会话才会阻止移除（只读检查）。仅指向该目录的、已不活跃的历史会话记录不会保留目录。
只读扫描绝不枚举历史会话、也绝不迁移会话；移除阶段则相反：删除目录前，它会读取该目录
的最新占用情况与 host 上报的谱系，并把该目录 lease 家族中的每个空闲 `ses_` 会话——lease
指名的会话以及由它们派生的子会话——送回仓库已核实的主 checkout。所有安全的家族成员
都会在开始等待前各请求一次，不会因为前一个立即处于 pending 而跳过后续兄弟；每次请求后
立即读取一次，只有实际 prune 才会在一次调用共享的 3 秒调度预算内（每 100ms 轮询，
绝不按目录或会话重置，绝不重复发送迁移请求）对全部仍 pending 的已接受迁移轮询确认，
预览与 close 从不等待。磁盘上已不存在的记录路径
计为 absent，其仍未终结的 lease 行只收敛一次为 `released`，重复运行不会改写已 released
行及其心跳。这些检查描述的是那一刻这一个连接服务的情况，而不是对之后
会话变化的保证。没有 lease 关联、也没有谱系能关联到 lease 根的会话会保留该目录且绝不被迁移；
占用列表不可用或不完整、
关联会话无法定位、返回了另一个会话或无法读取的位置、谱系不可读/不匹配/成环、迁移被拒绝、
排队或未生效、主
checkout 无法核实，以及期间开始运行的会话，都会保留目录。覆盖
范围就是这一个连接的服务，而非整机多服务清单。本地保护先于运行中会话查询判定，因此主
checkout 或 dirty 目录不会产生任何 host 调用；由于 prune 对运行时 host 的依赖是显式的，
运行中会话查询不可用、格式错误、ID 不匹配或返回非绝对路径，都会保留目录（即便是纯 CLI
候选）。人类可读的失败
计数只统计失败的 `git worktree remove` 调用，而非 host 异常。输出遵循全局
`--output` 选择：人类终端上为按目录统计的简洁文本，管道或 `--json` 时为机器可读聚合
结果。交互式终端上进度动画显示扫描与逐个移除检查，并在结果前清除；stdout 仅保留最终
文本或单个 JSON 文档。被透明 `timeout`、`env` 或 `command` 包裹的调用仍会获得受管
agent role。详见 [docs/opencode-api-lifecycle.md](docs/opencode-api-lifecycle.md)。

`phasegent plugin install` 部署的 OpenCode worktree 适配器是生成的单文件
dist。唯一真源为 `assets/opencode/src/`；用 `bun run build:plugin` 重建（`bun`
仅开发时需要），绝不手改仓库中的 dist `assets/opencode/phasegent-worktree.js` 或
已安装副本。插件不注册内嵌 skill，只注入简短的路由/安全提示；协议与角色参考是一个由
`phasegent skill install` 安装的文件系统 skill。适配器为每个会话保留精确的
worktree 注册：会话只会使用记录在自己 id 下的 worktree，子会话只继承其父会话的
目录，未知会话绝不被重定向到其他会话的 worktree。host 无法核实的既定迁移会让
prompt 失败，而不是在未核实的目录中运行。

## 许可证

Apache-2.0，详见 [LICENSE](LICENSE)。
