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

Forgejo provider 支持 issue 生命周期、comment 查询和仓库创建。
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
默认的 Forgejo。仓库命令支持 Forgejo 与 GitLab（Redmine 不支持仓库创建）。phasegent 不提供 CI/Actions 命令，通用 Forgejo CI 请使用 `fj`。

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
未设置时，resolver 会回退到 `config set` 持久化的 SQLite `global_setting`
行。该 token 也不会出现在任何 JSON 输出或日志中；命令默认使用安全提示，
也可以从 stdin 读取：

```text
phasegent config set redmine-git-mirror-api-key
phasegent config set redmine-git-mirror-api-key --stdin < /secure/path/plugin-bearer-key
```

plugin key 缺失或非法时 bootstrap 会以可操作的 config 错误失败。发送给
plugin 的 repository URL 默认是去掉凭据的 origin URL；通过设置
`PHASEGENT_REDMINE_REPOSITORY_URL` 可以指向自托管 mirror，而无需改动
本地 `origin` remote。该配置可以用 CLI 持久化：

```text
phasegent config set redmine-repository-url https://git.example.com/owner/repo.git
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

### 查找已有 Redmine project

另一台机器不需要预先知道已 bootstrap 的 Redmine project ID。先为任一 role
配置 API key 和 API base，然后列出该 Redmine 用户可见的 project：

```text
phasegent --role orchestrator --provider redmine project list
```

`project list` 不需要 `--project-id`；返回的 JSON 会包含每个 project 的数字
`id`、`name` 和 `identifier`。选择正确的 ID 后，在每次调用时显式使用
`--project-id`（project ID 不再持久化）：

```text
phasegent --role orchestrator --provider redmine --project-id 42 issue get 3
```

Project ID 仅作为单次调用参数，永远不会从 SQLite 或环境变量读取。
未提供 `--project-id` 且 provider 为 Redmine 时，会把当前 Git origin 与已有的
`redmine_git_mirror` 记录进行凭据无关的规范化匹配：唯一匹配时使用该 project，
多匹配时以有界的候选 id/name 列表失败并要求 `--project-id`，
发现过程中的 HTTP/鉴权/解析错误会直接传播。显式 `--project-id` 始终优先并跳过发现；
显式的 `--repository` 若与当前 origin 不一致，不会把 origin 静默匹配到该显式值。

四个 key 都配置好后，必须先为当前 repository 执行 bootstrap，然后才能执行
Redmine issue 的 create、search、update 或 close。命令从 `OWNER/REPOSITORY`
或当前 `origin` remote 派生 identifier，只复用完全匹配的 project，并按 role
保存关闭 status 的 ID（project ID 不再持久化）：

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
失败。

`issue search`/`create` 与 `version list` 在未提供 `--project-id` 时
（仅 Redmine，匹配当前 Git origin 的 `redmine_git_mirror` 记录）可自动推导
project。对于 `issue search`/`create`，唯一匹配时直接使用并完全绕过
bootstrap；无匹配时保持现有的自动 bootstrap；多匹配或任何发现阶段的
HTTP/鉴权/解析错误会在写入前以有界的可操作错误失败（列出候选 id/name，永不
暴露 URL/凭据）并要求 `--project-id`。对于 `version list`，唯一匹配时
列出该项目的版本，无匹配时返回可操作的错误提示使用 `--project-id` 或执行
`workflow bootstrap`，且该命令永远不会自动 bootstrap。显式 `--project-id`
始终优先；发现过程为只读且不会持久化 project id。

bootstrap 成功后，issue 和 journal 操作会使用已保存的 Redmine 配置：

```text
phasegent --role orchestrator --provider redmine issue create --title 'Plan' --body 'Details' --tracker Bug
phasegent --role orchestrator --provider redmine issue update-body 3 --body 'Updated plan' --tracker Feature
phasegent --role orchestrator --provider redmine status set 3 --status 'In Progress'
phasegent --role orchestrator --provider redmine status set 3 --status Resolved
phasegent --role orchestrator --provider redmine issue close 3
phasegent --role executor --provider redmine comment create 3 \
  --body '<!-- marker --> DONE' --marker '<!-- marker -->' --authorized
phasegent --role executor --provider redmine comment find-marker 3 --marker '<!-- marker -->'
phasegent --role orchestrator --provider redmine issue search --query 'phase'
phasegent --role executor --provider redmine version list
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

**计时边界：** SQLite ledger 是 wall-clock 时长的权威来源；rounded hours 只是
派生投影。provider 仅收到舍入后的摘要（Redmine）或通过 `add_spent_time` 传入
的精确秒数（GitLab）。本地 `elapsed_seconds` 除该有界投影外不会离开 SQLite。

**恢复与巡检（纯本地、有界）：**

```text
phasegent --role orchestrator --provider redmine timer list --status running --limit 20
phasegent --role orchestrator --provider redmine timer get <RUN_ID>
phasegent --role orchestrator --provider redmine timer recover <RUN_ID>
```

`list` 与 `get` 永远不会触及 provider 或网络。`list` 按
`running|finished|all` 过滤（默认 `all`），并以 `--limit` 限制条数（默认
100，最大 1000）。两者都不输出 secret 或完整 provider 响应，只返回 ledger 行
字段。`recover` 是显式孤儿路径：把已知 `running` 行标记为 `FAILED` 并复用与
`finish` 相同的 same-run provider 归一逻辑。它永不推断成功、永不重开已终止
行（并发 recover 安全），投影失败通过 `sync_status=failed`/`sync_error` 暴露，
而不会覆盖 FAILED 判定。

**永不猜测警告：** crash 后缺失的子进程 transcript 一律视为 `FAILED`。系统
永远不会从缺失状态猜测 `DONE/PARTIAL/BLOCKED`；只有操作员显式 `recover`
才会关闭孤儿。

**crash 与优雅 dispose：** hard-crash 残留会以 `running` 留在 SQLite，直到
你在重启后执行 `timer recover` —— 因此可通过 `timer list --status running`
巡检。插件的优雅 `dispose`（正常关闭）会通过有界重试把内存中活跃 run 以
`FAILED` 结束；若重试耗尽，dispose 会以 `warn` 记录确切的
`timer recover <RUN_ID>` 修复命令。

**Owner 元数据：** `timer start --owner-session-id/--owner-call-id` 记录拥有该
run 的 OpenCode 子代理 session/call（限 128 字符、去空、禁止控制字符）。旧库
中该列为 `NULL`，通过幂等的 `ALTER TABLE` 迁移添加；旧调用方保持兼容。Owner
字段仅本地、永不投影。

**投影租约：** `timer finish`/`recover` 以调用方绑定的租约 token
（`projection_token` + `projection_claimed_at`）认领 provider 投影，并在认领后
持有 `IMMEDIATE` SQLite 事务直至 token 绑定的终结写入。事务持有期间，并发
`BEGIN IMMEDIATE` 会因 `busy`/`locked` 直接返回“已在投影中”且永不 POST，因此
活跃投影不会因 provider 耗时超过固定墙钟常量而被窃取；只有持有者可将
`projecting` 终结为 `synced`/`failed`/`unconfirmed`。已加载的 `projecting` 若
token 不匹配绝不会视为本调用方持有。持有事务内的 hard-crash 会将 `projecting`
回滚为 `pending`/`failed`/`unconfirmed`（不留陈旧行）；遗留的 `projecting`
（旧版自动提交窗口或旧库）可通过 `timer recover` 显式将陈旧 `projecting`
（`NULL` 旧库或超过 `PROJECTION_LEASE_SECS`）强制重置为 `failed`，并**继续走
同一 token 绑定的投影重试路径**而非立即以 failed 返回，下一次重试按 marker
重新列表以保持幂等。

**不变量：** `projecting` 的进入与所有终态（`synced`/`failed`/`unconfirmed`）
均为原子且 owner 绑定（`projection_token` 校验）；未获所有权的调用方不会修改
该行。活动初始化（`list_time_entry_activities` + 落库）同样被序列化：先认领
再查询，且 `activity_id` 的持久化是 `WHERE projection_token = ? AND
sync_status='projecting'` 的 token 绑定更新，因此 `activity_id == NULL` 的两个
并发调用不会同时列表并 POST。`timer finish`/`timer recover` 的投影失败路径
已移除 round-3 的无 token 兜底写入：`sync_status` 仅由租约持有者独占地
修改。持有者已释放（回滚 / 未曾认领）时，行终态 `failed` 由 `finish_timer_run`
在 provider 之前本地持久化；投影失败通过 `record_failed_sync_error` 暴露，
该辅助函数仅修改非 `projecting` 行，永远不会冲掉并发活跃持有者。

**陈旧重置的活性保护：** `reset_stale_projection_to_failed` 自身先获取
`BEGIN IMMEDIATE`，因此当活跃投影仍持有自己的 `IMMEDIATE` 时，重置会被
`busy`/`locked` 阻塞。墙钟租约窗口仅作为崩溃恢复检查保留，用于持有者
未持有事务即崩溃（遗留 autocommit 或旧库）的 `projecting` 行。现代硬崩溃
在事务内则回滚认领，行不再处于 `projecting`；重置只在遗留或事务前孤儿上
真正生效。

**迁移与旧库：** `Storage::open` 在 `BEGIN IMMEDIATE` 事务内执行幂等 `ALTER
TABLE` 迁移，`busy`/`locked` 时有界重试；`COMMIT` 失败会回滚并向上抛错，初始化
永不在锁或提交失败时谎报成功。旧库 `NULL` 投影状态保持兼容；`NULL`
`claimed_at` 的 `projecting` 行被视为立即可恢复，需经显式的 `timer recover`
重试路径。

**hard-crash 语义：** provider POST 成功但 token 绑定的 `synced` 写入前的 crash
若在事务内则回滚认领（Redmine 下一次重试按 marker 归一避免重复；GitLab 因
无回读 marker 可能重复——已文档化，`unconfirmed` 用于缺 totals 场景）。缺失
transcript 绝不推断成功；孤儿保持 `running` 直至 `timer recover` 在任何 provider
查找前先持久化本地 `FAILED`。

**后台任务：** `task(background:true)` 的 executor/reviewer 委托会被跳过且不
创建 timer，同时通过 `client.app.log` 输出一条简洁 `warn`，标明 issue/role
并建议 `rerun foreground or use 'phasegent --role orchestrator --provider
redmine timer recover <RUN_ID>' if a previous run is orphaned`。其它跳过场景
（非 Redmine prompt、缺少上下文、explore/general）保持静默。

**same-run 重试与幂等：** `finish` 与 `recover` 共享基于 marker 的归一：同一
`run_id`/`result` 的第二次调用幂等，已终止行上以不同 result 重试会被拒绝。
有界重试（3 次、指数退避）安全，因为重试会在 POST 前按 run marker 重新列表；
Redmine 仍可防止重复，GitLab 已知的 crash 窗口（POST 成功但 SQLite 写入丢失）
仍被文档化——重试可能在 GitLab 上重复 spent time。

**二进制解析：** 插件按 `PHASEGENT_BIN`（若已设置且为可执行文件则优先）→
工作区 `target/debug/phasegent` 与 `~/.cargo/bin/phasegent`（仅当为常规可执行
文件）→ `PATH` 查询的顺序解析 `phasegent`。每次调用使用同一回退策略：自动
候选按序尝试，遇到 spawn 或兼容性失败（`unknown option`/`unrecognized`/
`incompatible`）时尝试下一个；无可用 binary 时返回有界结构化错误。测试中请设置
`PHASEGENT_BIN` 以获得确定性的 fake binary。

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
redmine)` provider、API base、repository、close status id 与 credential（project
ID 仅通过 `--project-id` 单次调用，不再持久化）；三个全局配置
（`PHASEGENT_REDMINE_GIT_MIRROR_API_KEY`、`PHASEGENT_REDMINE_REPOSITORY_URL`
以及机器级 `PHASEGENT_DEFAULT_PROVIDER`）保存在独立的 `global_setting` 表。

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
  close status id 与 credential 摘要（project ID 不再持久化，不会出现）。
  每条 credential 只输出 `present` 与 `length`，绝不输出明文。
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

### 使用 `config set` 持久化配置

普通 provider 命令只会从进程环境中读取 `PHASEGENT_*` 值作为运行时覆盖，
绝不会写回 SQLite。需要持久化时，用 `config set` 设置单个配置，用
`config clear` 删除配置：

```text
phasegent config set redmine-repository-url https://git.example.com/owner/repo.git
phasegent config clear redmine-repository-url
```

Project ID 不再持久化；每次调用显式使用 `--project-id`（例如
`phasegent --role orchestrator --provider redmine --project-id 42 issue get 3`）。
旧的 `redmine-project-id`、`gitlab-project-id`、`project-id` 别名会被视为未知
配置并拒绝。

配置名可以使用完整的 `PHASEGENT_*` 名称或 kebab-case 别名。全局配置
（`redmine-git-mirror-api-key`、`redmine-repository-url`、
`default-provider`）不需要 `--role`；role-scoped 配置需要 `--role`。
`config show` 继续提供 SQLite 的脱敏只读快照。

mirror plugin bearer token 属于 secret，绝不接受命令行值。默认使用安全
提示输入，也可以从 stdin 读取：

```text
phasegent config set redmine-git-mirror-api-key
phasegent config set redmine-git-mirror-api-key --stdin < /secure/path/plugin-bearer-key
```

非 secret 配置也支持 `--stdin`。值写入前会去除首尾空白，成功输出只包含
规范配置名和 role。

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

`config set` 支持的 role-scoped 配置：

- `PHASEGENT_PROVIDER` — 选择 `forgejo`、`redmine` 或 `gitlab`。
- `PHASEGENT_API_BASE` — 通用 API base；Forgejo 直接使用，Redmine 在
  provider-specific 变量未设置时作为兜底。
- `PHASEGENT_REPOSITORY` — Forgejo 使用的 `OWNER/REPOSITORY`。
- `PHASEGENT_REDMINE_API_BASE` — Redmine API base；优先于通用
  `PHASEGENT_API_BASE`。
- `PHASEGENT_REDMINE_CLOSE_STATUS_ID` — Redmine close status id。
- `PHASEGENT_CLOSE_STATUS_ID` — Redmine close status id 的通用别名。

Project-id 持久化（`PHASEGENT_REDMINE_PROJECT_ID`、
`PHASEGENT_GITLAB_PROJECT_ID`、`PHASEGENT_PROJECT_ID`）已在 Phase 1 移除；
请在每次调用时显式使用 `--project-id`。

`config set` 支持的全局配置：

- `PHASEGENT_REDMINE_GIT_MIRROR_API_KEY` — companion
  `redmine_git_mirror` Redmine plugin 的 bearer token。
- `PHASEGENT_REDMINE_REPOSITORY_URL` — 发给 mirror plugin 的
  repository URL 覆盖。
- `PHASEGENT_DEFAULT_PROVIDER` — 机器级默认 provider（`forgejo`、
  `redmine` 或 `gitlab`）。通过 `ProviderKind` 校验，避免拼写错误写入
  SQLite；可通过 `config set default-provider` 或
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

持久化全局 mirror 配置：

```text
phasegent config set redmine-git-mirror-api-key
```

仅以环境变量作为运行时覆盖，不写入数据库：

```text
export PHASEGENT_API_BASE=https://forgejo.example
export PHASEGENT_REPOSITORY=owner/repo
phasegent --role executor issue get 3
```

以上示例都不接受或打印任何 credential；provider credential 只能通过
`auth setup` 提供，mirror key 使用 `config set` 的安全提示或 stdin 路径，
绝不要在 shell 历史中以明文形式出现。

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
会回退到 `config set` 写入的 SQLite `global_setting` 行。该 token 属于部署级
密钥，建议通过安全提示或 stdin 持久化一次，而不是每次 bootstrap 都重新
提供。

然后查看可用界面：

```sh
phasegent --help
phasegent --role executor --help
phasegent --help issue
phasegent --help issue create
phasegent --help config provider
phasegent --help auth
phasegent --help workflow bootstrap
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
