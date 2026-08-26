# phasegent

`phasegent` 是面向 OpenCode、默认使用 Forgejo provider 的角色化 Rust CLI。
显式选择后也可以使用 Redmine issue 和 journal 操作。它只有一个 binary，
并通过显式 role 区分调用方：

```text
phasegent --role admin ...
phasegent --role orchestrator ...
phasegent --role executor ...
phasegent --role reviewer ...
```

Forgejo provider 支持 issue 生命周期、comment 查询、仓库创建和 CI 检查。
Redmine provider 支持 issue 生命周期（包括 tracker 选择和按校验名称或数字 ID
更新 status）、带 `#note-<id>` 锚点的 journal comment、project 查询与创建以及
issue status 查询。orchestrator-only 的 `timer` foundation 会记录每个
executor/reviewer phase 的一次 wall-clock run，并把舍入后的摘要投影到 Redmine
Time Entry。成功操作只输出紧凑 JSON；失败操作向 stderr 输出结构化 JSON
并返回非零状态。

```text
phasegent --role orchestrator issue get 3
phasegent --role orchestrator comment find-marker 3 --marker '<!-- marker -->'
phasegent --role executor comment create 3 --body '<!-- marker --> DONE' --marker '<!-- marker -->' --authorized
```

## Provider 选择

Redmine issue 和 comment 命令使用 `--provider redmine`。省略该参数时仍使用
默认的 Forgejo。仓库和 CI 命令仍然只支持 Forgejo。

Provider 解析优先级，从高到低：

1. 命令行显式 `--provider` 参数。
2. `PHASEGENT_PROVIDER` 环境变量（单进程覆盖）。
3. `PHASEGENT_DEFAULT_PROVIDER` 环境变量（持久化默认值的单进程覆盖）。
4. 平台标准 phasegent 数据库中持久化的
   `PHASEGENT_DEFAULT_PROVIDER` 行（参见
   [本地配置](#本地配置)；通过 `config provider set / get / clear` 管理）。
5. 由 `auth setup` 写入的角色级 `role_config.provider` 行。
6. Forgejo 兜底。

resolver 是只读的；`--provider` 是单命令覆盖，持久化默认值是机器级。可通过
这条优先级链在外部 Redmine 与 GitLab 环境之间切换，而不必逐条修改
角色级配置。

Redmine 需要在 **Administration > Settings > API** 中启用 REST API；不需要
JSONP 或 webhooks。通过 stdin 保存 API key：

```text
phasegent --role admin auth setup --stdin --provider redmine \
  --api-base https://redmine.example.com
```

该命令从 stdin 读取 key，并通过 [本地配置](#本地配置) 中描述的 SQLite
数据库将其持久化到当前 `$HOME`。`auth setup` 仍然只接受 stdin，key 永不
作为命令行参数、日志、shell history 或提交到仓库的内容出现；
`config show` 也只会把该行报告为 `present` 与 `length`。

除按 role 划分的 Redmine API key 外，`workflow bootstrap` 还会把当前
repository 注册到 companion 的 `redmine_git_mirror` Redmine plugin，由
plugin 异步 clone 并创建 Redmine 原生的 Git repository。plugin 使用单独的
bearer token，从 `PHASEGENT_REDMINE_GIT_MIRROR_API_KEY` 读取；当该环境变量
未设置时，resolver 会回退到 `config import-env` 持久化的 SQLite `global_setting`
行。该 token 也不会出现在任何 JSON 输出或日志中，建议导出一次后通过
`import-env` 落地：

```text
export PHASEGENT_REDMINE_GIT_MIRROR_API_KEY=<plugin-bearer-key>
phasegent --role admin config import-env
```

plugin key 缺失或非法时 bootstrap 会以可操作的 config 错误失败。发送给
plugin 的 repository URL 默认是去掉凭据的 origin URL；通过设置
`PHASEGENT_REDMINE_REPOSITORY_URL` 可以指向自托管 mirror，而无需改动
本地 `origin` remote：

```text
export PHASEGENT_REDMINE_REPOSITORY_URL=https://git.example.com/owner/repo.git
```

当 `--repository OWNER/REPOSITORY` 与本地 `origin` 不一致时必须设置
`PHASEGENT_REDMINE_REPOSITORY_URL`：如果静默把错误的 repository 发给
plugin，会注册一个 plugin 无法 clone 的 mirror。

每个 Forgejo repository 使用一个 Redmine project。Redmine 端必须已经存在三个
对应 agent role 的现有用户，每个用户各持有一个 API key：

- `admin` key 必须属于一个可以列出 role/user、并能创建或更新 project membership
  的 Redmine 用户；bootstrap 用它查询或创建 project，并写入所有 membership。
- `orchestrator`、`executor`、`reviewer` 三个 key 必须分别属于三个互不相同的
  Redmine 用户。bootstrap 通过 `/users/current.json` 配合对应的 role-scoped key
  解析每个 agent 身份，并按以下默认映射直接授予 project membership：

  | Role           | Redmine role |
  | -------------- | ------------ |
  | orchestrator   | Maintainer   |
  | executor       | Developer    |
  | reviewer       | Reporter     |

  以上是默认 role 名称；如果 Redmine 安装使用了本地化或自定义名称，需要在
  bootstrap 前安装好，只要名称完全匹配即可作为权威名称使用。

bootstrap 前先用 `auth setup --stdin` 保存按 role 划分的 API key。每次
setup 都需要带上 `--api-base https://redmine.example.com`，把
API base 写入该 role 的 SQLite 行；后续 issue、comment 命令会从已
保存的配置中读取 API base，不需要每次都设置 `PHASEGENT_API_BASE`
或传 `--api-base`：

```text
phasegent --role admin auth setup --stdin --provider redmine \
  --api-base https://redmine.example.com
phasegent --role orchestrator auth setup --stdin --provider redmine \
  --api-base https://redmine.example.com
phasegent --role executor auth setup --stdin --provider redmine \
  --api-base https://redmine.example.com
phasegent --role reviewer auth setup --stdin --provider redmine \
  --api-base https://redmine.example.com
```

四个 key 都配置好后，必须先为当前 repository 执行 bootstrap，然后才能执行
Redmine issue 的 create、search、update 或 close。命令从 `OWNER/REPOSITORY`
或当前 `origin` remote 派生 identifier，只复用完全匹配的 project，并按 role
保存 project 和关闭 status 的 ID：

```text
phasegent --role admin --provider redmine workflow bootstrap \
  --repository OWNER/REPOSITORY
```

如果不存在完全匹配的 project，bootstrap 会自动创建私有 project，然后为已有
的 orchestrator（Maintainer）、executor（Developer）、reviewer（Reporter）
用户调和直接 membership。Redmine 有多个关闭 status 时必须显式选择：

```text
phasegent --role admin --provider redmine workflow bootstrap \
  --repository OWNER/REPOSITORY --close-status-name Closed
```

bootstrap 只能由 admin 使用，且只支持 Redmine。只有唯一关闭 status 时会自动
选择。bootstrap 会在 Redmine project 上启用 `repository` module，以便
`redmine_git_mirror` plugin 挂载 Git mirror；并通过
`POST /sys/redmine_git_mirror/projects/<id>/repository` 加
`Authorization: Bearer <key>` 排队异步 mirror 任务，输出 JSON 中会包含
plugin 返回的 mirror 状态（`pending`、`cloning`、`ready`、`failed`，
若 bootstrap 只观察到一个已排队的 mirror 则为 `existing`）。`pending`
意味着异步 clone/fetch 任务已经入队，因此 bootstrap 视为成功；
bootstrap 会先调用
`GET /sys/redmine_git_mirror/projects/<id>/repository/mirror_<id>_<owner>_<repo>`，
若 mirror 已经存在则不再重复 POST。任何 agent 身份无法解析，其默认
role 名称在 Redmine 中找不到，或 mirror plugin 返回 HTTP 错误或
`failed` 状态时，bootstrap 都会在持久化任何身份映射之前以结构化错误
失败。bootstrap 成功后，issue 和 journal 操作会使用已保存的 Redmine 配置：

```text
phasegent --role orchestrator --provider redmine issue create --title 'Plan' --body 'Details' --tracker Bug
phasegent --role orchestrator --provider redmine issue update-body 3 --body 'Updated plan' --tracker Feature
phasegent --role orchestrator --provider redmine status set 3 --status 'In Progress'
phasegent --role orchestrator --provider redmine status set 3 --status Resolved
phasegent --role orchestrator --provider redmine issue close 3
phasegent --role executor --provider redmine comment create 3 \
  --body '<!-- marker --> DONE' --marker '<!-- marker -->' --authorized
phasegent --role executor --provider redmine comment find-marker 3 --marker '<!-- marker -->'
```

创建和更新 issue 时可以用 `--tracker` 显式选择 Redmine tracker：参数接受经过
校验的 tracker 名称（`Bug`、`Feature`）或数字 ID，会先对照 `/trackers.json`
解析；未知或有歧义的 tracker 会被拒绝。`status list` 列出当前 API key 可见的
status，`status set <ISSUE> --status NAME_OR_ID` 按校验过的名称或数字 ID 把
issue 移动到任意 status。Redmine note 输出会把每条 journal comment 锚定到
`#note-<id>`，审计引用可以直接指向具体 note；`comment get` 仍然可以完整读取
单条 note。

创建和更新 issue 还可以在同一次请求中携带 Redmine 原生规划字段：`--parent-issue`、
`--fixed-version`、`--start-date`、`--due-date`、`--estimated-hours` 和
`--done-ratio`。`--fixed-version` 会按精确的版本名称或数字 ID 对照当前配置
project 的版本解析，非法取值会在写入前被拒绝。省略这些 flag 时请求负载保持
原有格式不变。`version list` 用于发现当前 API key 可见的 project 版本：

```text
phasegent --role orchestrator --provider redmine issue create \
  --title 'Plan' --body 'Details' --fixed-version 'Sprint 1' \
  --parent-issue 12 --start-date 2026-08-01 --done-ratio 0
phasegent --role orchestrator --provider redmine issue update-body 3 \
  --body 'Updated plan' --done-ratio 40
phasegent --role executor --provider redmine version list
```

Redmine issue 关系由三个子命令管理。`relation list <ISSUE>` 列出某个 issue
的全部关系，并从该 issue 的视角渲染（当该 issue 是关系目标时显示逆方向名称
`blocked` 和 `follows`，从源头看则显示 `blocks`、`precedes` 和 `relates`）。
`relation create <ISSUE> --to <ISSUE> --type blocks|precedes|relates [--delay N]`
从第一个 issue 指向 `--to` 创建关系；`--type` 只接受正向规范名称（逆方向名称
`blocked` 和 `follows` 不被接受，因此关系永远不会被反向创建），`--delay N`（非负
整数天数）仅在 `--type precedes` 时有效。`relation delete <RELATION_ID>` 按数字
ID 删除关系。relation list 对 orchestrator、executor、reviewer 可读；create 和
delete 仅限 orchestrator；admin 角色被拒绝全部三种操作；Forgejo 会在任何网络访问
之前返回结构化的 not-supported 错误。

```text
phasegent --role executor --provider redmine relation list 3
phasegent --role orchestrator --provider redmine relation create 3 --to 5 --type blocks
phasegent --role orchestrator --provider redmine relation create 3 --to 5 --type precedes --delay 2
phasegent --role orchestrator --provider redmine relation delete 9
```

### Phase timer ledger

orchestrator 在 executor/reviewer 子调用前后启动和结束本地 execution ledger。
`timer start` 是纯本地操作：它会先把 `run_id`、issue、phase、agent role、
attempt 和 wall-clock 开始时间写入 SQLite，然后才进行任何 provider 或网络
操作。自动生成或显式传入的 `run_id` 同时也是重试时的稳定 marker。

```text
phasegent --role orchestrator --provider redmine timer start 28 \
  --phase implementation --agent-role executor --attempt 1 --run-id phase-5a-28
phasegent --role orchestrator --provider redmine timer finish phase-5a-28 --result DONE
```

ledger 保留精确的整秒 elapsed time，Redmine hours 则向上取整到 0.01（任何正
时长最低为 0.01 小时）。finish 会列出 time-entry activity，依次优先精确的
`AI automation`、精确的 `Development`，最后才是唯一 default activity。找不到
首选名称、首选名称重复，或 default activity 多个时都会返回结构化配置错误，
绝不使用任意 activity 静默分类。finish 仅支持 Redmine 且只允许 orchestrator。
每次 posting 前都会按稳定 run marker 重新列表检查，因此 204/empty create
响应可以安全重试而不会创建重复 Time Entry；201 响应会解码并关联
`redmine_time_entry_id`。

底层 project metadata 命令仍然需要显式确认：

```text
phasegent --role admin --provider redmine project create \
  --name 'OpenCode workflow' --identifier opencode-workflow --confirm
```

orchestrator 可以读取、搜索、创建、更新和关闭 issue，可以按校验过的名称或
数字 ID 设置 issue status，并管理 comment。
admin 可以列出和创建 Redmine project，以及列出 issue status。executor 和
reviewer 可以读取 issue、comment、Redmine project、issue status 和 project
版本，并且只能创建明确授权的带 marker comment。

`--role` 是能力策略和工作流路由标签，不是硬身份边界：能够执行 binary 的
调用方也可以选择其他 role。持久化 token 和 provider 配置按 role 分离；安全边界
仍然是 token 文件权限和 Forgejo token scope。

可以使用 `--api-base` 和 `--repository OWNER/REPOSITORY` 显式配置 API 与仓库，
也可以使用 `PHASEGENT_API_BASE`、`PHASEGENT_REPOSITORY`，或从 `origin` git
remote 解析。SSH remote 使用主机名和 HTTPS，不会把 SSH transport 端口当作 API
端口；HTTPS remote 会保留显式配置的端口。

### 以 `-` 开头的选项值

两参数形式 `--option value` 会拒绝以 `-` 开头的 value（否则解析器无法判断它是新 flag）。
计划文本中常出现 `- Goal` 或 `---` 这类 Markdown 内容，因此上述选项也支持内联形式
`--option=value`：

```text
phasegent --role orchestrator issue create --title 'Plan' --body='- Goal'
phasegent --role orchestrator issue update-body 3 --body=---
phasegent --role executor comment create 3 --body=--- --marker='<!-- marker -->' --authorized
phasegent --role orchestrator issue search --query=-tag:regression
phasegent --role executor comment find-marker 3 --marker=---
```

内联形式仅作为 leading-dash 场景的逃生口；普通 value 仍可使用两参数形式，
当严格 missing-value 检测触发时，结构化错误信息会提示使用内联形式。

## 分支级 Redmine Issue 上下文

Redmine issue 上下文是原生本地 Git config，而不是 SQLite 状态。绑定一个
issue 会把

```text
[branch "feature/name"]
    redmine-issue-id = 123
```

写入 `.git/config`。该绑定只属于当前 checkout 和当前分支：它不会被推送，
也不会自动共享，切换分支时活动的 issue 上下文会随分支一起切换。

这些命令只操作当前 checkout，不需要任何 provider 访问：

```text
phasegent issue bind 123
phasegent issue bind 123 --replace
phasegent issue unbind
phasegent issue status
phasegent hooks install
```

`issue bind` 为当前命名分支保存绑定；detached HEAD 会被拒绝。重复绑定同一
issue 是 no-op；已有不同绑定时会被拒绝，除非显式传入 `--replace`。
`issue unbind` 移除当前分支的绑定；没有绑定时是 no-op。`issue status`
输出当前分支及其绑定的 issue（如果存在）。

### 托管 Git hooks

只有当当前 checkout 的 `origin` 与 bootstrap 的 `OWNER/REPOSITORY` 完全
一致时，`workflow bootstrap` 才会自动安装托管的 `prepare-commit-msg` 和
`commit-msg` hooks。在错误的 checkout 中 bootstrap 另一个 repository 时不会
安装本地 hooks。`hooks install` 可以随时执行同样的安装；它是纯本地操作，
不涉及 role、provider、凭据和网络。

无关的既有 hooks 会被保留：它们被移动到
`.git/hooks/phasegent-original/<hook-name>`，托管 wrapper 会链接到原脚本，
让原始 hook 仍然先运行。托管安装是幂等的；重复执行只会原地更新托管
wrapper。

Hook 行为：

- 在已绑定的命名分支上，普通与 template 提交消息会被追加一次且仅一次
  `Refs #<id>`。
- merge、squash 或 cherry-pick（`commit`）来源生成的消息永远不会被改写。
- 提交消息引用了与分支绑定不同的 Redmine issue 时，提交校验失败；重复的
  生成 trailer 同样被拒绝。
- 默认关系始终是 `Refs`。hooks 绝不会自动添加 `Fixes`；当提交应当关闭
  issue 时，请在消息中显式使用 `Fixes #<id>`。
- `git commit --no-verify` 会绕过本地 hooks。它仍然是显式的逃生口，但不应该
  被 agent 工作流使用。

### Issue 生命周期集成

- Redmine `issue create` 成功后，会在可能时自动把返回的 issue 绑定到当前
  匹配的分支。
- Redmine `issue close` 成功后，仅当当前分支的绑定恰好指向该 issue 时才会
  解除绑定。
- detached HEAD、绑定不一致、非 Git 目录以及本地失败都不会撤销远端成功的
  结果；它们只会输出一条有界的 warning。
- Forgejo 的 issue create/close 不会触碰 Redmine 分支上下文。

## 本地配置

`auth setup`、按 role 划分的 provider 配置、phase execution ledger 以及两个
`redmine_git_mirror` plugin 配置都集中保存在同一个 SQLite 数据库中。数据库
位于 `directories::ProjectDirs` 给出的 OS 标准配置目录（限定符
`com` / `Cloud1ful` / `phasegent`）：

```text
Linux :   ~/.config/phasegent/phasegent.sqlite3
macOS :   ~/Library/Application Support/com.Cloud1ful.phasegent/phasegent.sqlite3
Windows: %APPDATA%\Cloud1ful\phasegent\config\phasegent.sqlite3
```

增量创建的 `execution_timer_runs` 表保存精确的整秒 elapsed time、rounded
hours，以及可选的 activity/Time Entry 投影状态。Unix 上目录以模式 `0700`
创建、数据库文件以模式 `0600` 创建，避免非属主用户读取。

`auth setup` 出于设计会把 Forgejo role
token 与 Redmine API key 以明文形式写入 SQLite。CLI 永不输出 credential：
仍然只接受 stdin 输入、结构化错误只提示缺失的变量名、`config show` 永不输出
明文 secret。role 隔离保持不变：每个 role 各自保留自己的 `(forgejo,
redmine)` provider、API base、repository、project id、close status id 与
credential；三个全局配置（`PHASEGENT_REDMINE_GIT_MIRROR_API_KEY`、
`PHASEGENT_REDMINE_REPOSITORY_URL` 以及机器级
`PHASEGENT_DEFAULT_PROVIDER`）保存在独立的 `global_setting` 表。

### 使用 `config show` 查看数据库

`config show` 输出脱敏后的本地 SQLite JSON 快照。不需要 `--role`，默认
覆盖全部 role；带上 `--role ROLE` 可只查看某一个 role：

```text
phasegent config show
phasegent --role executor config show
```

输出字段：

- `database_path` — SQLite 数据库的绝对路径。
- `roles[]` — 每个 role 一条记录，包括 provider、Forgejo/Redmine URL、
  project id、close status id 与 credential 摘要。每条 credential 只输出
  `present` 与 `length`，绝不输出明文。
- `global_settings[]` — `PHASEGENT_REDMINE_GIT_MIRROR_API_KEY`（只显示存在
  与否与长度）、`PHASEGENT_REDMINE_REPOSITORY_URL`（输出脱敏后的 URL：
  去掉内嵌的 userinfo、密码、query 与 fragment；SSH 风格的 `user@host` 视为
  SSH transport 信息会保留，不当作 credential）以及
  `PHASEGENT_DEFAULT_PROVIDER`（非敏感字面量）。
- `global_default_provider` — 持久化 `PHASEGENT_DEFAULT_PROVIDER` 的顶层
  别名；一条 `config show` 命令即可同时输出 resolver 关心的默认。默认从未
  设置时输出 `null`。

`config show` 永不输出明文 secret。如果操作员确实需要读出明文 key 值，
本工具不支持该工作流；secret 始终保留在 SQLite 内部并仅由 credential
读取路径使用。

### 使用 `config import-env` 持久化环境变量

普通 provider 命令只会从进程环境中读取 `PHASEGENT_*` 值，绝不会写回
SQLite。`config import-env` 是显式地把当前已设置的受支持环境变量写入
数据库的命令；由于大多数写入的字段都按 role 隔离，必须带上 `--role
ROLE`：

```text
phasegent --role orchestrator config import-env
```

命令输出 JSON 报告，逐项列出 role-scoped 变量与全局设置，对 secret
字段标记而不输出明文，并附带 `imported` 与 `skipped` 计数。命令不会修改
进程环境变量；空值会被跳过，`import-env` 不会用 `export PHASEGENT_*=""`
之类的空字符串覆盖已有字段。

两个全局 plugin 设置可以在同一次调用中一并持久化，避免每次运行 `workflow
bootstrap` 的 shell 都要带上同一份 plugin key：

```text
export PHASEGENT_REDMINE_GIT_MIRROR_API_KEY=<plugin-bearer-key>
export PHASEGENT_REDMINE_REPOSITORY_URL=https://git.example.com/owner/repo.git
phasegent --role admin config import-env
```

### 优先级与受支持的环境变量

解析优先级遵循 **CLI 参数 > 环境变量 > SQLite > origin/default
discovery**，role-scoped 配置与全局 plugin 配置都遵循同一规则。环境变量
继续作为运行时覆盖生效：只有在同名环境变量未设置或为空时才会回退读取
SQLite。provider 选择遵循同一模式，但在 `PHASEGENT_PROVIDER` 运行时覆盖
与角色级 `role_config.provider` 之间多了一个机器级默认：

1. 显式 `--provider` 参数
2. `PHASEGENT_PROVIDER` 环境变量
3. `PHASEGENT_DEFAULT_PROVIDER` 环境变量
4. 持久化的 `PHASEGENT_DEFAULT_PROVIDER`（通过
   `config provider set / get / clear` 设置）
5. 角色级 `role_config.provider`（由 `auth setup` 写入）
6. forgejo 兜底

`config import-env` 持久化的 role-scoped 变量：

- `PHASEGENT_PROVIDER` — 选择 `forgejo`（默认）或 `redmine`。
- `PHASEGENT_API_BASE` — 通用 API base；Forgejo 直接使用，Redmine 在
  provider-specific 变量未设置时作为兜底。
- `PHASEGENT_REPOSITORY` — Forgejo 使用的 `OWNER/REPOSITORY`。
- `PHASEGENT_REDMINE_API_BASE` — Redmine API base；优先于通用
  `PHASEGENT_API_BASE`。
- `PHASEGENT_REDMINE_PROJECT_ID` — Redmine project id。
- `PHASEGENT_REDMINE_CLOSE_STATUS_ID` — Redmine close status id。
- `PHASEGENT_PROJECT_ID` — Redmine project id 的通用别名。
- `PHASEGENT_CLOSE_STATUS_ID` — Redmine close status id 的通用别名。

`config import-env` 持久化的全局设置：

- `PHASEGENT_REDMINE_GIT_MIRROR_API_KEY` — companion
  `redmine_git_mirror` Redmine plugin 的 bearer token。
- `PHASEGENT_REDMINE_REPOSITORY_URL` — 发给 mirror plugin 的
  repository URL 覆盖。
- `PHASEGENT_DEFAULT_PROVIDER` — 机器级默认 provider（`forgejo`、
  `redmine` 或 `gitlab`）。通过 `ProviderKind` 校验，避免拼写错误写入
  SQLite；可通过 `config import-env` 或
  `config provider set / get / clear` 子命令管理。

### 管理全局默认 provider

机器级默认保存在 `global_setting` 中，由三个子命令管理。这些子命令都不
要求 `--role`，因为该默认是全局而非角色级：

```text
phasegent config provider get         # 未设置时输出 null
phasegent config provider set gitlab # 校验后持久化
phasegent config provider clear      # 删除已持久化行
```

`config provider get` 输出持久化字面量，未设置时输出 `null`；取值只可能是
`forgejo` / `redmine` / `gitlab`。`config provider set` 通过
`ProviderKind::from_str` 校验字面量并持久化；非法值在写入 SQLite 之前
返回结构化 config 错误。`config provider clear` 删除该行，让其回退到角色
级 provider；删除成功时返回 `{"cleared": true}`，默认已为 absent 时
返回 `{"cleared": false}`。同一默认值也会出现在 `config show` 输出中：
既在顶层 `global_default_provider` 字段，也在 `global_settings[]` 列表
里，与现有角色级快照保持一致。

### 实用命令

在不打印任何 secret 的前提下检查数据库是否存在：

```text
test -f "$HOME/.config/phasegent/phasegent.sqlite3" \
  && echo "phasegent SQLite database exists" \
  || echo "run any phasegent command to initialise it"
```

输出全部或单个 role 的脱敏快照：

```text
phasegent config show
phasegent --role executor config show
```

把指定 role 的 role-scoped 与全局环境变量持久化到数据库：

```text
phasegent --role orchestrator config import-env
phasegent --role admin config import-env
```

仅以环境变量作为运行时覆盖，不写入数据库：

```text
export PHASEGENT_API_BASE=https://forgejo.example
export PHASEGENT_REPOSITORY=owner/repo
phasegent --role executor issue get 3
```

以上示例都不接受或打印任何 credential；secret 只能通过 `auth setup`
提供，绝不要在 shell 历史中以明文形式出现。

## 安装

在本目录执行：

```sh
cargo install --path .
```

从安全交互输入或 stdin 配置 Forgejo role token：

```sh
phasegent --role orchestrator auth setup
phasegent --role executor auth setup --stdin
```

Forgejo token 与 Redmine API key 都保存在
`~/.config/phasegent/phasegent.sqlite3` 中，目录 Unix 权限
`0700`、文件权限 `0600`。CLI 永不接受命令行 token/key、永不回显、永不
进入任何渲染过的 JSON；`auth setup` 只读取 stdin，`config show` 只
把相应行报告为 `present` 与 `length`。绝不要传递、记录或提交任一
credential。

`workflow bootstrap` 通过 `PHASEGENT_REDMINE_GIT_MIRROR_API_KEY` 解析
companion 的 `redmine_git_mirror` plugin bearer token，环境变量未设置时
会回退到 `config import-env` 写入的 SQLite `global_setting` 行。该 token
属于部署级密钥，建议导出一次后落地到 SQLite，而不是每次 bootstrap 都重新
导出。

然后查看可用界面：

```sh
phasegent --help
phasegent --role executor --help
phasegent --help issue
phasegent --help issue create
```

## 发布

推送版本标签 `vX.Y.Z`（例如 `v0.1.0`）即可触发公开的 GitHub Release（仅标签触发，不会因分支推送发布）：

```sh
git tag vX.Y.Z
git push origin vX.Y.Z
```

`.github/workflows/release.yml` 使用 `sccache` 构建 5 个 `phasegent` 二进制：

- `x86_64-unknown-linux-gnu` 构建于 `ubuntu-24.04`
- `aarch64-unknown-linux-gnu` 构建于 `ubuntu-24.04-arm`
- `aarch64-apple-darwin` 构建于 `macos-14`
- `x86_64-pc-windows-msvc` 构建于 `windows-2022`
- `aarch64-pc-windows-msvc` 构建于 `windows-2022`（通过 `ilammy/msvc-dev-cmd`，`arch: amd64_arm64`）

每个二进制以 `phasegent-<tag>-<target>` 命名（Windows 为 `*.exe`），并在 Ubuntu 发布任务中生成 `SHA256SUMS` 校验文件。发布通过 `softprops/action-gh-release` 完成并自动生成 release notes。

远程编译缓存是可选的。如需为 `sccache` 启用 Cloudflare R2，请在 GitHub 仓库的 Secrets 中配置以下 4 项（工作流内无任何硬编码值，`SCCACHE_REGION` 固定为 `auto`）：

- `R2_ACCESS_KEY_ID`
- `R2_SECRET_ACCESS_KEY`
- `SCCACHE_ENDPOINT`
- `SCCACHE_BUCKET`

工作流会把 `R2_ACCESS_KEY_ID` / `R2_SECRET_ACCESS_KEY` 映射为 `AWS_ACCESS_KEY_ID` / `AWS_SECRET_ACCESS_KEY`，并透传 `SCCACHE_BUCKET` / `SCCACHE_ENDPOINT`。未配置这些 Secrets 时，`sccache` 会回退到本地磁盘缓存，CI 与发布仍可正常执行。当前不发布任何 Docker 或容器镜像。

本项目使用 Apache-2.0 许可证，详见 [LICENSE](LICENSE)。
