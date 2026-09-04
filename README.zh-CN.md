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

使用 Redmine 时，可由管理员准备 project 和 role membership：

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
未指定时使用 Forgejo。

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

成功命令返回紧凑 JSON；错误写入 stderr，并以非零状态退出。

## 许可证

Apache-2.0，详见 [LICENSE](LICENSE)。
