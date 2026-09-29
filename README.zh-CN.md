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
PHASEGENT_ROLE=orchestrator phasegent admin auth setup

# 从受保护文件或其他安全来源读取
PHASEGENT_ROLE=executor phasegent admin auth setup --stdin < /secure/path/token

# 显式选择其他 provider 及其 API base
PHASEGENT_ROLE=orchestrator phasegent --provider redmine admin auth setup \
  --stdin --api-base https://redmine.example.com

# 准备 Redmine 的 project 和 role membership
PHASEGENT_ROLE=admin phasegent --provider redmine admin workflow bootstrap \
  --repository OWNER/REPOSITORY
```

CLI 从 `PHASEGENT_ROLE` 环境变量解析 role：受管 OpenCode session 按次导出，
其他宿主在 shell 中设置。PowerShell 下使用
`$env:PHASEGENT_ROLE='orchestrator'; phasegent ...`，而不是 `NAME=value` 前缀。

## 常用命令

```sh
PHASEGENT_ROLE=orchestrator phasegent issue search --query "bug"
PHASEGENT_ROLE=orchestrator phasegent issue get 123
PHASEGENT_ROLE=orchestrator phasegent issue get 123 124 125
PHASEGENT_ROLE=orchestrator phasegent comment list 123
PHASEGENT_ROLE=orchestrator phasegent issue create \
  --title "Short title" --body "Issue details"
PHASEGENT_ROLE=orchestrator phasegent issue update 123 --body "Updated details"
PHASEGENT_ROLE=orchestrator phasegent issue close 123
PHASEGENT_ROLE=executor phasegent issue status
PHASEGENT_ROLE=executor phasegent issue branches 123
phasegent doctor
```

分支关联是本地只读查询：`issue status` 显示当前分支及其关联 issue 与缓存状态，`issue branches N` 列出本仓库中关联到该 issue 的全部分支（跨全部 scope）。

选择的 provider 不是默认值时，在命令上添加 `--provider redmine` 或
`--provider gitlab`。可以使用 `--repository OWNER/REPOSITORY` 和
`--project-id ID` 覆盖仓库或 project 的自动发现。

## Issue 层级

父子关联使用 provider 原生机制：Redmine 子任务（`parent_issue_id`）和
GitLab Work Item 层级（Epic→Issue、Issue→Task）。层级不是 issue relation，
关闭父 issue 永不级联关闭子 issue——每个子 issue 保留自己的状态与审计记录。

```sh
# 读取一个条目的父 issue 和至多 50 个直接子 issue
PHASEGENT_ROLE=executor phasegent hierarchy get 641

# 设置 / 清除原生父级（仅 orchestrator）
PHASEGENT_ROLE=orchestrator phasegent hierarchy set --parent 640 --child 641
PHASEGENT_ROLE=orchestrator phasegent hierarchy unset --child 641
```

ID 是 provider 特定的：Redmine ID 是 issue ID，GitLab ID 是层级输出中显示的
数字全局 Work Item ID。当 50 个子条目的上限截断了子列表时，`hierarchy get`
会报告 `children_truncated: true`。Redmine 专用的
`issue create/update --parent-issue` 旗标在写入时设置同一原生字段；类型化的
`hierarchy` 命令是覆盖 GitLab 的 provider 感知入口。

父 issue 是伞形索引：承载总体目标与验收标准；每个子 issue 承载自己的聚焦
目标、约束、验收标准与审计记录。

Provisioning（`auth setup`、config 写操作、`workflow bootstrap`）位于
人类操作者专用的 `admin` 组（`phasegent admin ...`），AI role 永不调用。

完整命令参考见 `phasegent --help`（或 `phasegent --help <topic>`），OpenCode
skill 见 `skills/phasegent`：它选择 tracking 模式（`INLINE` /
`TRACKED_ISSUE` / `LOCAL_ISSUE`），通过 `--provider` 从配置解析 provider
（redmine、gitlab 或 local；缺失或过期选择会直接失败并给出可操作的指引），
并记录当前的 `issue update`、`worktree acquire --base`、`worktree probe`
与 `worktree prune` 用法。

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
PHASEGENT_ROLE=orchestrator phasegent worktree acquire --issue 123 --base main
PHASEGENT_ROLE=executor phasegent worktree probe --issue 123
```

`phasegent plugin install` 部署的 OpenCode worktree 适配器是生成的单文件
dist。真源为 `assets/opencode/src/` 和 `skills/phasegent/` 的提示词文件；用
`bun run build:plugin` 重建（`bun` 仅开发时需要），绝不手改仓库中的 dist
`assets/opencode/phasegent-worktree.js` 或已安装副本。

## 许可证

Apache-2.0，详见 [LICENSE](LICENSE)。
