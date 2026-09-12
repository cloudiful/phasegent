# phasegent

[English](README.md)

`phasegent` 是面向 OpenCode provider 工作流的角色化 CLI。它用一套命令行
接口处理 issue、仓库、评论和工作流自动化。

## 功能

- 支持 Forgejo（默认）、Redmine 和 GitLab。
- 支持本地 provider（`--provider local`），离线使用，无需凭证与网络。
- 支持 `admin`、`orchestrator`、`executor`、`reviewer`、`tester` 角色。
- 按 provider 支持 issue 搜索、创建、更新、关闭，以及评论、状态、关系、
  版本、project 列表（参见 [Provider 能力矩阵](#provider-能力矩阵)）。
- 本地 issue 索引（默认 SQLite，可选 PostgreSQL）。
- 本地分支与 issue 绑定，以及托管 Git hooks。
- 支持 stdio / streamable HTTP 的 MCP 服务，复用同一套角色权限。
- 成功输出紧凑 JSON，错误输出结构化信息。

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

## OpenCode 集成

`phasegent` 随附一个 OpenCode skill，位于 `skills/phasegent-workflow`。在
OpenCode 一侧它只是薄入口，将工作委托给 CLI：选择 tracking 模式、委派给
executor/reviewer/tester，并落实 marker、VERDICT、note-pointer 以及由
orchestrator 独占的 timer/status/notify 契约。OpenCode 一侧不会重新实现
CLI；`phasegent --role <role>` 始终是权威的语法参考。

安装方式是将 skill 树复制到用户级 skills 目录：

```sh
cp -r skills/phasegent-workflow ~/.config/opencode/skills/
```

之后 skill 会依据其 `name: phasegent-workflow` 的 frontmatter 加载。tracking
可以落在 Redmine issue（多阶段 `REDMINE_ISSUE`）、本地 provider issue
（`--provider local`，它替代 `.opencode/plans/*.md` markdown），或用于
琐碎只读工作的 inline。

## 快速开始

Forgejo 是默认 provider。为需要使用 CLI 的每个 role 配置 credential。credential
通过安全提示或 stdin 读取，不接受命令行明文参数。

```sh
# 安全提示输入
phasegent --role orchestrator admin auth setup

# 从受保护文件或其他安全来源读取
phasegent --role executor admin auth setup --stdin < /secure/path/token
```

显式选择其他 provider 及其 API base：

```sh
phasegent --role orchestrator --provider redmine admin auth setup \
  --stdin --api-base https://redmine.example.com
```

准备 Redmine 的 project 和 role membership：

```sh
phasegent --role admin --provider redmine admin workflow bootstrap \
  --repository OWNER/REPOSITORY
```

## 常用命令

```sh
phasegent --role orchestrator issue search --query "bug"
phasegent --role orchestrator issue get 123
phasegent --role orchestrator issue get 123 124 125
phasegent --role orchestrator comment list 123
phasegent --role orchestrator issue create \
  --title "Short title" --body "Issue details"
phasegent --role orchestrator issue update 123 --body "Updated details"
phasegent --role orchestrator issue close 123
phasegent doctor
```

默认使用 Forgejo；需要时在命令上添加 `--provider redmine` 或
`--provider gitlab`。可以使用 `--repository OWNER/REPOSITORY` 和
`--project-id ID` 覆盖仓库或 project 的自动发现。

Provisioning（`auth setup`、config 写操作、`workflow bootstrap`）位于
人类操作者专用的 `admin` 组（`phasegent admin ...`），AI role 永不调用。
`phasegent doctor` 无需 role 即可报告 credential 存在性（指纹而非明文）
和索引状态。

### 一次性 Markdown 正文（`--body-file`）

`issue create`、`issue update` 与 `comment create` 支持以
`--body-file PATH` 代替 `--body`，长 Markdown 无需经过 shell。两个参数互斥；
`--body-file` 只接受普通文件，最大 2 MiB，且必须为有效 UTF-8。

文件在 provider 解析、项目发现和任何网络访问之前完成本地读取与校验，provider
收到的正是文件内容。成功写入后默认删除文件，除非传入 `--keep-body-file`；任何
读取、校验、参数或 provider 失败都会保留文件。读取后被替换或修改的路径永不删除，
只会输出有界 warning。

```sh
phasegent --role orchestrator issue create --title "Plan" --body-file /tmp/plan.md
phasegent --role orchestrator issue update 123 --body-file /tmp/plan.md
phasegent --role executor comment create 123 --body-file /tmp/audit.md \
  --marker "<!-- ai-executor ... -->" --authorized
```

查看可用命令：

```sh
phasegent --help
phasegent --help issue
phasegent --help admin
```

## Provider 能力矩阵

`phasegent` 面向四个 provider（`forgejo`、`redmine`、`gitlab`、
`local`）。下表与 `src/policy.rs` 一致；所有 CLI/MCP 守卫与
dispatcher 分支都对齐这张表。单元格为 `yes` 表示该 provider 已实现
对应能力，`no` 表示在触及任何网络/文件前返回结构化的
`not_supported` 错误。

| 能力 | Forgejo | Redmine | GitLab | Local |
|---|:---:|:---:|:---:|:---:|
| IssueRead / IssueSearch / IssueCreate / IssueUpdateBody / IssueClose | yes | yes | yes | yes |
| IssueAttachmentUpload | no | **no** | no | no |
| CommentCreate / CommentRead / CommentFindMarker | yes | yes | yes | yes |
| RepoCreate | yes | no | yes | no |
| ProjectRead | no | yes | yes | yes |
| ProjectCreate | no | yes | no | yes |
| IssueStatusRead | no | yes | yes | yes |
| VersionRead | no | yes | yes | yes |
| RelationRead / RelationCreate / RelationDelete | no | yes | yes | no |

### IssueAttachmentUpload — 统一 not-supported

所有 provider 都拒绝 `issue upload-attachment`（退出码 1，
`not_supported`）。该能力保留（orchestrator 或 tester），以便未来
phase 在不重命名能力的前提下重新启用底层上传路径。证据改走评论
或外部链接。

### GitLab 读侧对等（Phase 2）

原本对 GitLab 保持 `no` 的三行已用等价读侧补齐：

- `ProjectRead` → `GET /projects`，映射到 `RedmineProject`。
- `IssueStatusRead` → 静态 `WORKFLOW_LABELS` 目录，映射到
  `RedmineIssueStatus`（GitLab 没有原生 status 枚举，工作流以项目
  label 编码）。
- `VersionRead` → `GET /projects/:id/milestones`，映射到
  `RedmineVersion`。

GitLab 的 `ProjectCreate` 保持 `no`，因为等价路径是 `repo create`
（`POST /projects`），且按设计只有这一个入口。Phase 2 加宽了 GitLab
的 `ApiIssue` DTO，可解码 `milestone`、`due_date`、`weight`、
`time_stats`、`assignee(s)`、`created_at`、`updated_at`，同时保留旧
字段为 required，确保旧 fixture 与审计评论消费方继续可用。

### Planning 标志的例外

`--tracker`、`--parent-issue`、`--fixed-version`、`--start-date`、
`--due-date`、`--estimated-hours`、`--done-ratio` 在 CLI 上接受，但
按 provider 不同方式转发或拒绝：

- Redmine：每个标志都是原生字段。`--fixed-version` 按精确名称或
  数字 id 在已配置的 project 内解析。
- GitLab：`--estimated-hours` 通过原生 `time_estimate` 端点转发；
  `--tracker` 映射为 `type::bug` / `type::feature` label；其他
  planning 标志一律拒绝。
- Forgejo：拒绝所有 planning 标志。
- Local：为保持解析兼容而全部接受，但不持久化（本地索引只保存
  title、body、state）。

### 写侧 relation 自动（Phase 3）

在 Redmine 或 GitLab 上使用 `issue create --parent-issue <ID>` 时，
lifecycle 助手会自动创建一条从新建子 issue 指向父 issue 的
`relates` 关联，具备幂等性（通过 `list_relations` /
`list_issue_links` 检测已存在的 `relates` 关联，不会重复创建）。
助手在失败时（parent id 为 `0`、自指、provider 错误）返回受控的
warning，不会污染 stdout JSON 或 exit code。AI 工作流不需要单独
调用 `relation create`；`status set`、`status advance`、`issue close`
也已挂载同一钩子，但目前传入 `None`（因为读侧 DTO 还未暴露 parent
linkage），助手返回 `Skipped` 静默。Forgejo 和 Local 没有 relation
面，助手直接跳过。

## 桌面应用

普通的 `phasegent <command>` 调用仍为 CLI。显式打开桌面应用：

```sh
phasegent gui
```

在终端中无参数运行 `phasegent` 仍显示帮助。从 Explorer/Finder（无终端）
启动则会打开桌面应用。

发布包：Windows 提供一个 x64 MSI 及对应的 raw exe；macOS 提供 `.dmg`
中的未签名应用，首次打开时 Gatekeeper 可能会提示。

## 配置

`admin auth setup` 将 credential 保存在本地。`config show` 提供脱敏视图，
永远不会打印 secret。

```sh
phasegent config show
phasegent config provider get
phasegent admin config provider set redmine
phasegent admin config provider clear
```

可以通过单次命令的 `--provider` 或环境变量 `PHASEGENT_PROVIDER` 选择
provider。稳定的非 secret 配置可直接编辑 `phasegent.toml`（可用
`PHASEGENT_CONFIG_PATH` 覆盖其位置）。有效优先级为 CLI 参数 > 环境变量 >
TOML > SQLite > 默认值。

使用 PostgreSQL 作为 issue 索引后端时，通过 stdin 配置 URL：

```sh
phasegent admin config set index-pg-url --stdin
```

## 本地 provider

`--provider local` 完全离线运行，无需凭证、网络，也不需要 `admin auth setup`
的 token。它使用配置数据库旁边一个独立的本地数据库文件
（`phasegent-local.sqlite3`），默认填充 project、issue、评论以及标准状态
转移。`admin auth setup --provider local` 仅记录按 role 划分的 provider 偏好，
永不提示输入 secret：

```sh
phasegent --role executor --provider local admin auth setup
phasegent --role executor --provider local issue create \
  --title "Local task" --body "Works offline"
```

issue、评论和状态相关命令在本地后端均可使用（`issue search`、
`issue get`、`issue create`、`issue update`、`issue close`、
`comment create`、状态 list/next/advance/set，以及 project list/create）。
仓库创建、附件上传以及 relation 操作会返回结构化的 `not_supported`
错误；`version list` 返回空目录，与能力矩阵一致。返回的 envelope
遵循 Redmine 对齐的形状，因此选择 `--provider local` 的脚本能获得
稳定格式。

当索引所使用的非空 `PHASEGENT_INDEX_PG_URL` 已设置时选择 PostgreSQL，
否则使用 SQLite。同一时刻只有一个本地后端处于活动状态（single-active，
不会双写）。`migrations/pg/0002_local.sql` 下的加法迁移与 SQLite 结构保持
一致。

## 本地分支上下文

将 issue 绑定到当前 Git 分支，并在本地安装托管 hooks：

```sh
phasegent issue bind 123
phasegent issue status
phasegent hooks install
phasegent issue unbind
```

这些命令只操作本地 checkout，不需要访问 provider。

## Worktree

按 issue 隔离 worktree，让多个任务共享同一仓库而不互相冲突。lease 以
`(repo, issue, session)` 为键。session 的解析顺序为：`--session`（issue close
用 `--worktree-session`）、`PHASEGENT_SESSION_ID`、最后是兼容性的
`phasegent` 回退值。legacy 回退只会在 stderr 输出警告，并可能让并发的
session 共用同一 lease，因此 OpenCode 工作流必须在每个 session 开始时生成
一个稳定 id，并在所有 worktree 操作中复用。自动隔离默认关闭：冲突时
`acquire` 复用当前 checkout 并发出警告。用
`admin config set worktree-auto true` 或单次 `--isolate` 开启。当 `git status`
探测本身失败时，dirty 状态未知：开启自动隔离会创建新 worktree，关闭则复用
当前 checkout，两种情况都会在 stderr 警告 —— 未知状态绝不会被静默当作干净。
新 worktree 需要环境文件时请手动复制 `.env`。

```sh
export PHASEGENT_SESSION_ID="<stable-session-id>"
phasegent --role orchestrator worktree acquire --issue ISSUE --session "$PHASEGENT_SESSION_ID" --isolate
phasegent --role orchestrator worktree heartbeat --lease LEASE --session "$PHASEGENT_SESSION_ID"
phasegent --role executor worktree status --issue ISSUE
phasegent --role executor worktree list
phasegent --role orchestrator worktree prune --stale-days 14
phasegent --role orchestrator worktree prune --stale-days 14 --release-stale --reason "stale session recovery"
phasegent --role orchestrator worktree prune --stale-days 14 --remove
phasegent --role orchestrator worktree release --lease LEASE
```

`heartbeat` 只会刷新 stored session 与调用方一致、且仍为 active 的 lease；
session 不匹配或已终结的 lease 会返回结构化冲突且行内容保持不变。
`prune` 默认是只读报告：列出 heartbeat 早于 `--stale-days`（默认 14）的
active lease 以及所有可删除的 worktree，不做任何修改。`--release-stale` 必须
同时给出 `--reason TEXT`，把恰好这些 stale active lease 转为 `retained` 并
记录原因；`--remove` 只删除同时满足 `retained`、已过期、且干净的 worktree
（若同时请求恢复，则先执行恢复）。两个动作都需显式开启；没有
`--release-stale` 而给出 `--reason` 会被拒绝。任何动作都不会删除分支，且
`git worktree remove` 永远不会带 `--force`，因此 dirty worktree、active lease
和未提交内容都不会被删除。
关闭 issue 只释放当前 repo、issue、session 匹配的 lease：远程关闭失败不会改变
任何本地 lease，没有已解析 session 时不会释放任何 lease，也不会影响其他
session。

## MCP 服务

通过 Model Context Protocol 对外提供约定的操作：

```sh
# Stdio（默认，供本地 MCP 客户端使用）
phasegent --role executor mcp serve

# Streamable HTTP，挂载于 /mcp
phasegent --role executor mcp serve --transport http --bind 127.0.0.1:3000
```

工具以启动时的 `--role` 和 `--provider` 参数运行；MCP 客户端永远不需要
（也不能）提供 role。

## 通知

通知仅支持手动发送：无自动触发。 通过 `notify send`（CLI）或
`notify_send`（MCP）显式发送：

```sh
phasegent --role executor notify send --event completion --title "Done" --body "Details"
```

事件：`completion`、`blocked`、`failure`、`interruption_suspected`、
`publish_failed`。通过 `admin config set notify-enabled true` 和
`admin config set notify-channel <name>` 配置；secret 须经 `--stdin` 传入。admin 组仅限人类操作者。

## 容器镜像

纯 CLI 镜像，以非 root 用户运行。默认命令在回环地址上提供需认证的
streamable HTTP MCP；状态数据持久化在 `/data` 下。镜像随版本标签（`v*`）
发布为 `<tag>` 和 `latest`。请将 `OWNER/REPO` 替换为实际仓库 slug：

```sh
docker pull ghcr.io/OWNER/REPO:latest
mkdir -p ./phasegent-data
docker run --rm -p 127.0.0.1:3000:3000 \
  -e PHASEGENT_MCP_AUTH_TOKEN="$(cat /secure/path/mcp-token)" \
  -v ./phasegent-data:/data \
  ghcr.io/OWNER/REPO:latest
```

- Token：通过 `-e` 传入 `PHASEGENT_MCP_AUTH_TOKEN`，不会烘焙进镜像。
  未设置时 HTTP 会在绑定前直接退出。
- 存储：`/data` 为 volume；默认 `PHASEGENT_DB_PATH=/data/phasegent.sqlite3`，
  `PHASEGENT_CONFIG_PATH=/data/phasegent.toml`。
- 本地 MCP 客户端可用 stdio 覆盖（stdout 保持协议干净，诊断信息走 stderr）：
  `docker run --rm -i -v ./phasegent-data:/data ghcr.io/OWNER/REPO:latest --role executor mcp serve --transport stdio`
- 警告：默认仅绑定回环地址。使用 `--bind 0.0.0.0:3000` 会将 HTTP 暴露到回环之外：
  请妥善保管 token，并配合防火墙或反向代理。

成功命令返回紧凑 JSON；错误写入 stderr，并以非零状态退出。

## 许可证

Apache-2.0，详见 [LICENSE](LICENSE)。
