# phasegent

[English](README.md)

`phasegent` 是面向 OpenCode provider 工作流的角色化 CLI。它用一套命令行
接口处理 issue、仓库、评论和工作流自动化。

## 功能

- 支持 Forgejo（默认）、Redmine 和 GitLab。
- 支持本地 provider（`--provider local`），离线使用，无需凭证与网络。
- 支持 `admin`、`orchestrator`、`executor`、`reviewer`、`tester` 角色。
- 按 provider 支持 issue 搜索、创建、更新、关闭，以及评论、状态、关系、版本
  和附件操作。
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

## 快速开始

Forgejo 是默认 provider。为需要使用 CLI 的每个 role 配置 credential。credential
通过安全提示或 stdin 读取，不接受命令行明文参数。

```sh
# 安全提示输入
phasegent --role orchestrator auth setup

# 从受保护文件或其他安全来源读取
phasegent --role executor auth setup --stdin < /secure/path/token
```

显式选择其他 provider 及其 API base：

```sh
phasegent --role orchestrator --provider redmine auth setup \
  --stdin --api-base https://redmine.example.com
```

准备 Redmine 的 project 和 role membership：

```sh
phasegent --role admin --provider redmine workflow bootstrap \
  --repository OWNER/REPOSITORY
```

## 常用命令

```sh
phasegent --role orchestrator issue search --query "bug"
phasegent --role orchestrator issue get 123
phasegent --role orchestrator issue create \
  --title "Short title" --body "Issue details"
phasegent --role orchestrator issue update-body 123 --body "Updated details"
phasegent --role orchestrator issue close 123
```

默认使用 Forgejo；需要时在命令上添加 `--provider redmine` 或
`--provider gitlab`。可以使用 `--repository OWNER/REPOSITORY` 和
`--project-id ID` 覆盖仓库或 project 的自动发现。

查看可用命令：

```sh
phasegent --help
phasegent --help issue
phasegent --help auth
```

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

`auth setup` 将 credential 保存在本地。`config show` 提供脱敏视图，
永远不会打印 secret。

```sh
phasegent config show
phasegent config provider get
phasegent config provider set redmine
phasegent config provider clear
```

可以通过单次命令的 `--provider` 或环境变量 `PHASEGENT_PROVIDER` 选择
provider。稳定的非 secret 配置可直接编辑 `phasegent.toml`（可用
`PHASEGENT_CONFIG_PATH` 覆盖其位置）。有效优先级为 CLI 参数 > 环境变量 >
TOML > SQLite > 默认值。

使用 PostgreSQL 作为 issue 索引后端时，通过 stdin 配置 URL：

```sh
phasegent config set index-pg-url --stdin
```

## 本地 provider

`--provider local` 完全离线运行，无需凭证、网络，也不需要 `auth setup`
的 token。它使用配置数据库旁边一个独立的本地数据库文件
（`phasegent-local.sqlite3`），默认填充 project、issue、评论以及标准状态
转移。`auth setup --provider local` 仅记录按 role 划分的 provider 偏好，
永不提示输入 secret：

```sh
phasegent --role executor --provider local auth setup
phasegent --role executor --provider local issue create \
  --title "Local task" --body "Works offline"
```

issue、评论和状态相关命令在本地后端均可使用（`issue search`、
`issue get`、`issue create`、`issue update-body`、`issue close`、
`comment create`、状态 list/next/advance/set，以及 project list/create）。
仓库与附件操作会返回结构化的 `not_supported` 错误，与当前能力一致。
返回的 envelope 遵循 Redmine 对齐的形状，因此选择 `--provider local`
的脚本能获得稳定格式。

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

按 issue 隔离 worktree，让多个任务共享同一仓库而不互相冲突。自动隔离
默认关闭：冲突时 `acquire` 复用当前 checkout 并发出警告。用
`config set worktree-auto true` 或单次 `--isolate` 开启。新 worktree
需要环境文件时请手动复制 `.env`。

```sh
phasegent --role orchestrator worktree acquire --issue 239 --session alpha
phasegent --role executor worktree status --issue 239
phasegent --role executor worktree list
phasegent --role orchestrator worktree release --lease lease-...
phasegent --role orchestrator worktree prune --stale-days 14 --dry-run
```

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
`publish_failed`。通过 `config set notify-enabled true` 和
`config set notify-channel <name>` 配置；secret 须经 `--stdin` 传入。

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
