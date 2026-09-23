# phasegent

[English](README.md)

`phasegent` 是面向 OpenCode provider 工作流的角色化 CLI，用一套命令行接口
处理 issue、仓库、评论和工作流自动化。

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
phasegent --role orchestrator admin auth setup

# 从受保护文件或其他安全来源读取
phasegent --role executor admin auth setup --stdin < /secure/path/token

# 显式选择其他 provider 及其 API base
phasegent --role orchestrator --provider redmine admin auth setup \
  --stdin --api-base https://redmine.example.com

# 准备 Redmine 的 project 和 role membership
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

选择的 provider 不是默认值时，在命令上添加 `--provider redmine` 或
`--provider gitlab`。可以使用 `--repository OWNER/REPOSITORY` 和
`--project-id ID` 覆盖仓库或 project 的自动发现。

Provisioning（`auth setup`、config 写操作、`workflow bootstrap`）位于
人类操作者专用的 `admin` 组（`phasegent admin ...`），AI role 永不调用。

完整命令参考见 `phasegent --help`（或 `phasegent --help <topic>`），OpenCode
skill 见 `skills/phasegent`：它选择 tracking 模式（`INLINE` /
`TRACKED_ISSUE` / `LOCAL_ISSUE`），通过 `--provider` 从配置解析 provider
（最终回退 Forgejo），并记录当前的 `issue update` 与 `worktree prune` 用法。

`phasegent plugin install` 部署的 OpenCode worktree 适配器是生成的单文件
dist。真源为 `assets/opencode/src/` 和 `skills/phasegent/` 的提示词文件；用
`bun run build:plugin` 重建（`bun` 仅开发时需要），绝不手改仓库中的 dist
`assets/opencode/phasegent-worktree.js` 或已安装副本。

## 许可证

Apache-2.0，详见 [LICENSE](LICENSE)。
