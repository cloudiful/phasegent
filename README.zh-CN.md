# phasegent

[English](README.md)

`phasegent` 是面向 OpenCode provider 工作流的角色化 CLI，用一套命令行接口
处理 issue、结构化 agent record、评论和工作流自动化。

## 安装

要求：Rust stable 和 Cargo。

```sh
git clone https://forgejo.cloud1ful.com/tools/phasegent.git
cd phasegent
cargo install --path .
```

需要 PostgreSQL 索引后端时，启用可选 feature：

```sh
cargo install --path . --features postgres
```

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

选择的 provider 不是默认值时，在命令上添加 `--provider redmine` 或
`--provider local`。可以使用 `--project-id ID` 覆盖 project 的自动发现；
`--repository OWNER/REPOSITORY` 指定 Git 主机上的仓库，用于
`workflow bootstrap` 与 project 发现。

Provisioning（`auth setup`、config 写操作、`workflow bootstrap`）位于
人类操作者专用的 `admin` 组（`phasegent admin ...`），AI role 永不调用。

完整命令参考见 `phasegent --help`（或 `phasegent --help <topic>`），OpenCode
skill 见 `skills/phasegent`：它选择 tracking 模式（`INLINE` /
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
`issue close` 会先通过 OpenCode API（`opencode api session.move`）把结束会话送回
仓库主 checkout，再移除目录；无法确认时保留目录，待 `issue sync` 确认会话已转移后
再移除。`phasegent worktree prune` 是独立的显式移除路径，不受这些保护约束。详见
[docs/opencode-api-lifecycle.md](docs/opencode-api-lifecycle.md)。

`phasegent plugin install` 部署的 OpenCode worktree 适配器是生成的单文件
dist。真源为 `assets/opencode/src/` 和 `skills/phasegent/` 的提示词文件；用
`bun run build:plugin` 重建（`bun` 仅开发时需要），绝不手改仓库中的 dist
`assets/opencode/phasegent-worktree.js` 或已安装副本。

## 许可证

Apache-2.0，详见 [LICENSE](LICENSE)。
