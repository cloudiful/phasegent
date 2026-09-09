# phasegent

[English](README.md)

`phasegent` 是面向 OpenCode provider 工作流的角色化 CLI。它用一套命令行
接口处理 issue、仓库、评论和工作流自动化。

## 功能

- 支持 Forgejo（默认）、Redmine 和 GitLab。
- 支持 `admin`、`orchestrator`、`executor`、`reviewer`、`tester` 角色。
- 按 provider 支持 issue 搜索、创建、更新、关闭，以及评论、状态、关系、版本
  和附件操作。
- `issue search` 自动预热本地 issue 索引，并在 provider 失败时提供按范围过滤的
  stale 本地回退。
- 支持本地分支与 issue 绑定，以及托管 Git hooks。
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

使用 Redmine 时，管理员仅需 admin API key 即可准备 project 和 role membership：

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

角色 credential 始终保存在本地且只写：可在 Settings 页面或通过
`auth setup` 输入，已保存的 key 不会被显示。发布包：Windows 提供一个 x64
MSI 及对应的 raw exe；macOS 提供 `.dmg` 中的未签名 Tauri 应用，首次打开时
Gatekeeper 可能会提示。

Windows 安装包快捷方式选项：MSI 会显示 Shortcut Options 页面。默认勾选
Start Menu，不勾选 Desktop。两个快捷方式均以 `phasegent gui` 启动已安装
应用。静默安装保持默认；需要时覆盖：

```sh
msiexec /i phasegent-<tag>-x86_64-pc-windows-msvc.msi /qn
msiexec /i phasegent-<tag>-x86_64-pc-windows-msvc.msi /qn PHASEGENT_DESKTOP_SHORTCUT=1
msiexec /i phasegent-<tag>-x86_64-pc-windows-msvc.msi /qn PHASEGENT_STARTMENU_SHORTCUT=0
```

升级保持相同的 per-user 安装标识；卸载会删除快捷方式、Start Menu 文件夹
和用户 `PATH` 条目。

## 配置

`auth setup` 将 provider credential 保存在本地配置数据库中。`config show` 提供
脱敏视图，永远不会打印 secret。

```sh
phasegent config show
phasegent config provider get
phasegent config provider set redmine
phasegent config provider clear
```

可以通过单次命令的 `--provider` 或环境变量 `PHASEGENT_PROVIDER` 选择 provider。
未指定时使用 Forgejo。完整优先级见 `phasegent --help config provider`：CLI >
环境变量 > TOML > SQLite > 默认值。

Redmine `workflow bootstrap` 仅需 admin API key。它会通过 admin API 查找或创建
内置 orchestrator、executor、reviewer、tester 用户，并将其 key 保存在本地
SQLite；生成的 credential 始终保留在 SQLite，不应写入 TOML。

稳定的非 secret 配置可直接编辑 `phasegent.toml`（默认 ProjectDirs 配置目录，
可用绝对路径 `PHASEGENT_CONFIG_PATH` 覆盖）。有效优先级为 CLI 参数 > 环境变量 >
TOML > SQLite > 默认值。TOML 为只读叠加层；`config set`/`clear` 与
`config provider set`/`clear` 仍只写 SQLite，TOML 值会一直覆盖 SQLite，直到文件
（或环境变量）被移除。

Issue 搜索优先访问 provider，并自动预热本地索引。provider 请求失败时，非空查询
可以使用按范围过滤的 stale 本地结果。默认索引后端是 SQLite。使用 PostgreSQL
时，先以 `postgres` feature 安装，再通过 stdin 配置 URL：

```sh
phasegent config set index-pg-url --stdin
```

## 本地分支上下文

将 issue 绑定到当前 Git 分支，并在本地安装托管 hooks：

```sh
phasegent issue bind 123
phasegent issue status
phasegent hooks install
phasegent issue unbind
```

这些命令只操作本地 checkout，不需要访问 provider。

## 阶段计时

阶段计时属于**内部自动管理**：随着 `status set` / `status advance` 的状态
流转按段累计耗时，并在 `issue close` 时收尾。仅 orchestrator 拥有合法触发权，
AI 工作流**不应**直接调用 timer CLI。

`timer` 命令组仍然保留，作为**手动兜底与恢复**专用面 —— 用于查看本地
ledger、手动收尾生命周期未自动关闭的 run，或恢复孤儿记录：

```sh
# 查看本地阶段 run（只读，仅本地）
phasegent --role orchestrator timer list
phasegent --role orchestrator timer get <RUN_ID>

# 手动兜底 / 恢复（仅运维使用）
phasegent --role orchestrator timer start <ISSUE> --phase NAME \
  --agent-role executor|reviewer|tester --attempt N
phasegent --role orchestrator timer finish <RUN_ID> \
  --result DONE|PARTIAL|BLOCKED|FAILED
phasegent --role orchestrator timer recover <RUN_ID>
```

`timer start` / `timer finish` 仍仅限 orchestrator。`list` / `get` 不接触
provider；`finish` / `recover` 投影到 Redmine 或 GitLab（Forgejo 两者均拒绝），
失败不会让底层状态流转或关闭操作失败 —— 失败仅以 stderr 警告形式输出。
无论哪种 role，MCP 都不暴露 timer 操作。

## MCP 服务

通过 Model Context Protocol 对外提供约定的操作：

```sh
# Stdio（默认，供本地 MCP 客户端使用）
phasegent --role executor mcp serve

# Streamable HTTP，挂载于 /mcp
phasegent --role executor mcp serve --transport http --bind 127.0.0.1:3000
```

工具以启动时的 `--role` 和 provider 参数运行；MCP 客户端永远不需要
（也不能）提供 role。约定工具：`capabilities`、`issue_get`、
`issue_search`、`status_next`、`comment_create` 和 `notify_send`。
除 server role 为 orchestrator 外，`comment_create` 需要服务端
`--authorized`。`status_advance`、timer 和角色提升永远不会暴露。

## 通知

通知仅支持手动发送：无自动触发，其他命令不会发送通知。通过
`notify send`（CLI）或 `notify_send`（MCP）显式发送：

```sh
phasegent --role executor notify send --event completion --title "Done" --body "Details"
```

事件：`completion`、`blocked`、`failure`、`interruption_suspected`、
`publish_failed`。标题/正文会被截断（140/2000 字符），发送前会先持久化
意图。通过 `config set notify-enabled true` 和
`config set notify-channel <name>` 配置；secret 须经 `--stdin` 传入。
未启用或未配置时仍会持久化一条 skipped 记录并输出
`{"notified": false}`，不会失败；投递失败返回结构化通知错误。适用于
`orchestrator`、`executor`、`reviewer` 和 `tester`。

## 容器镜像

纯 CLI 镜像（无 GUI 依赖），以非 root 用户运行。默认命令在回环地址上
提供需认证的 streamable HTTP MCP，未设置 bearer token 时直接拒绝启动；
状态数据持久化在 `/data` 下。镜像仅在版本标签（`v*`）时发布为
`ghcr.io/OWNER/REPO:<tag>` 与 `ghcr.io/OWNER/REPO:latest`；请将
`OWNER/REPO` 替换为 GitHub 仓库 slug。Dockerfile 为纯运行时镜像：
CI 在原生 runner 上按架构以
`cargo build --release --bin phasegent --no-default-features` 构建 CLI，
将产物暂存于 `ci-image-input/phasegent`，Dockerfile 仅 `COPY` 该预构建
产物（Docker 内无 Rust 工具链、无 `cargo build`，也无 QEMU 编译 Rust）。
每个 `v*` 标签都会发布按架构划分的镜像，并合并多架构 manifest，
覆盖 `<tag>` 与 `latest`。本地执行 `docker build` 前需先按同样方式
暂存产物：先按上述命令构建 CLI，再将二进制复制到
`ci-image-input/phasegent`。

```sh
docker pull ghcr.io/OWNER/REPO:latest
mkdir -p ./phasegent-data
docker run --rm -p 127.0.0.1:3000:3000 \
  -e PHASEGENT_MCP_AUTH_TOKEN="$(cat /secure/path/mcp-token)" \
  -v ./phasegent-data:/data \
  ghcr.io/OWNER/REPO:latest
```

- Token：通过 `-e`（或 secrets 管理器）传入
  `PHASEGENT_MCP_AUTH_TOKEN`；不要作为命令行参数传递，也不会烘焙进镜像。
  未设置时 HTTP 会在绑定前直接退出。
- Role/provider 保留在服务端：默认是 `--role executor`，如需变更请覆盖
  镜像 CMD，客户端永远不提供 role：
  `docker run ... ghcr.io/OWNER/REPO:latest --role executor --provider redmine mcp serve --transport http --bind 127.0.0.1:3000`
- 存储：`/data` 为 volume；默认
  `PHASEGENT_DB_PATH=/data/phasegent.sqlite3`，
  `PHASEGENT_CONFIG_PATH=/data/phasegent.toml`。请挂载
  `-v ./phasegent-data:/data`，或用 `-e` 同时覆盖这两个路径。
- 本地 MCP 客户端可用 stdio 覆盖（stdout 保持协议干净，诊断信息走 stderr）：
  `docker run --rm -i -v ./phasegent-data:/data ghcr.io/OWNER/REPO:latest --role executor mcp serve --transport stdio`
- 警告：默认仅绑定回环地址。使用 `--bind 0.0.0.0:3000`
  （配合 `-p 0.0.0.0:3000:3000`）会将已认证的 HTTP 暴露到回环之外：
  请妥善保管 bearer token，配合防火墙或反向代理，且未设置
  `PHASEGENT_MCP_AUTH_TOKEN` 时不要对外发布。

成功命令返回紧凑 JSON；错误写入 stderr，并以非零状态退出。

## 许可证

Apache-2.0，详见 [LICENSE](LICENSE)。
