# Design — workflow-first `arkforge` 命令面

本文件给出 CHG-2026-CLI-arkforge-workflow-first-cli 的完整行为定义。
authority/permit/`arkforged` execution journal/rescue 语义以
chg-agent-native-cli 的 design.md 为准。本文定义前端组合、信息推断、
交互、输出契约，以及一个不进入 mechanics evidence 的 CLI authority
approval record。

## 0. 三个使用平面

~~~text
人类（TTY）        arkforge flash fw.tar.gz          1 条命令 + 1 屏确认
Agent（强/已知目标） flash plan → apply                2 次调用
Agent（弱 Loader）   device list → plan → apply  3 次调用，显式 profile/device
CI（全输入钉死）  flash run --file … --ack …        1 条命令，永不提示
~~~

三个平面共享同一实现路径（supervisor 的 assess/materialize/apply 原语），
区别只在信息如何被收集（询问 / 推断 / 显式参数）与如何被呈现。

## 1. 全局行为

### 1.1 交互门（determinism 规则的精确化）

允许交互（列表选择、确认屏、waiting 提示）当且仅当：

~~~text
stdin、stdout 与 stderr 均为 TTY
AND --output human（含默认）
AND 未给 --no-input
~~~

其余情况（任一标准流被管道/重定向、CI、`--output json|jsonl`、
`--no-input`）下，缺失必要信息是
typed refusal，错误信封携带已解析的全部事实与 `next_commands`。
"同一调用在终端、CI 与 Agent 进程中行为一致"的原承诺由此收窄为
"在所有非交互环境中行为一致"，且交互仅限 proposal 定义的三类必要信息。

### 1.2 runtime 自动确保

任何需要 runtime 的命令在 runtime 未运行时自动执行等价于 `daemon start` 的
启动（读取 `config` 中的 HDC/profile-file 项），并在人类模式打印一行
提示、JSON 模式在结果文档中记录 `runtime_autostarted: true`。
`--no-auto-start` 恢复现行的 typed refusal。`AUTHORITY_ALREADY_PAIRED`
拒绝语义不变——自动确保永不接管 ArkDeck 已配对的 runtime。

HardwareCampaign 不读取持久配置。需要 campaign 的 `flash plan/run`、`apply`
必须对当次调用显式给 `--hardware-campaign <id>`：无 runtime 时以该 ID
启动；已运行 runtime 必须具有相同 ID，否则拒绝
`RUNTIME_CAMPAIGN_MISMATCH`，不自动停止或重启。无此参数时也不得继承一个
已运行的 campaign runtime 来物化新 plan，而是要求显式给出同一 ID。

自动确保是并发幂等的：同一 runtime-dir 的竞争者在 owner-only 启动锁下
只有一个创建 runtime，其余命令在校验精确 config/campaign 一致后附着；
不一致为 typed refusal。命令 help 仍使用原 `effect` 表示业务/设备 effect，
并新增 `runtime_effect: none|may-start-service`，避免把自动启动误报为纯读。

### 1.3 选择器语法

- `--device <observation-id>`：精确绑定，全平面可用（plumbing 语义不变）。
- `--target <selector>`：porcelain 人类/脚本选择器，按序尝试解析为
  (1) 完整序列号；(2) 序列号或 observation-id 的唯一前缀（≥ 4 字符）；
  (3) 由强模型身份事实得到的 productModel 名（当且仅当在场唯一）。
  不得把“唯一兼容 profile 声明的 model”当作已观测 productModel。
  解析成功即绑定该精确 observation；
  歧义 → TTY 弹选择器 / 非交互 exit 6 refusal 并列出全部候选。
  两个参数互斥。依据 chg-agent-native-cli 的命名规则，精确 ID 与模糊选择器
  不重载同一参数。
- `flash run` 的位置参数 `FILE` 仅在交互门开启时接受，且必须是既存普通文件；
  结构化模式必须使用 `--file` / `--artifact`（解析器不猜位置参数，规则保持）。

### 1.4 复合输出与部分事实

- 所有 JSON 结果按各 schema 定义的有界 projection 内嵌被引用资源的
  **摘要**（denormalized），而非仅 ID；不无界复制全量 event/receipt。
- 所有错误信封在 v1 字段（`code`/`message`/`remediation`/`retryable`/
  `next_commands`）之外新增 `facts`：失败前已完成阶段产出的复合事实
  （已导入的 artifact、已识别的设备、已产出的 assessment……），
  使失败路径与成功路径信息等价——Agent 不需要补查询。
- `facts` 是 `arkforge.command-result/v1.error` 的可选加性字段；旧字段含义不变。
  每个命令的 help 必须声明 `facts` 内各 projection 的 schema 与最大条数。
- `next_commands` 一律是可直接执行的完整命令行（现行风格保持）。

## 2. 推断引擎

### 2.1 设备解析

~~~text
候选 := 当前 observations
        （给定固件时）∩ 模式身份与兼容 profile 的 usbIdentities 相容
        （给定 --target/--device 时）∩ 选择器命中
if |候选| = 1  → 绑定；输出识别块（见 2.2）
if |候选| = 0  → 交互：waiting-for-device（按已推断 profile/mode 过滤，Ctrl-C 退出）
                 非交互：refusal DEVICE_NOT_FOUND（--wait-device <ms> 可选等待）
if |候选| > 1  → 交互：编号选择器（型号/模式/序列号/总线位置/身份强度）
                 非交互：refusal DEVICE_AMBIGUOUS，facts 携带全部候选
~~~

### 2.2 profile 兼容性与物理型号识别

~~~text
固件侧：artifact 解析格式 → artifactCompatibility.formats 命中的 profile 集
USB 侧：observation 的 VID/PID/mode → usbIdentities 命中的 profile 集
          （VID/PID 只证明模式/协议兼容，永不单独证明板子）
探针侧：native DEVICE_INFO / HDC 事实（device list --deep 或绑定前复核）
compatible_profiles := 三者中“有能力声明兼容性”的集合交集
  空/多 → refusal PROFILE_AMBIGUOUS，列出各信号候选集
  恰一 → compatible_profile 恰一，但尚不得由此推出 physical model

physical_model := 只使用 profile 声明为 model-binding 的强事实
  （例如 HDC productModel 或未来的设备唯一型号事实）
  恰一且与 compatible_profile 一致 → model 已识别，strength: strong
  否则 → model: null，strength 保持实际证据等级

--profile 显式给出时仍须通过固件、USB/mode 与已知 probe 事实的相容性
检查（profile_resolution: explicit）；它是操作者决策，不是新的设备证据。
~~~

识别结果必须分开“兼容 profile”与“物理型号”，携带证据链与
强度，禁止只报结论。下例是 normal/HDC 强识别：

~~~json
"identification": {
  "model": "DAYU200",
  "profile": "org.openharmony.dayu200@1.0.0",
  "profile_resolution": "inferred",
  "evidence": ["artifact-format:rockchip-images-targz",
               "usb-mode:hdc-normal",
               "hdc-product-model:DAYU200"],
  "strength": "strong"
}
~~~

已知边界（如实呈现，不掩饰）：

- DAYU200 与 DAYU600 在已注册 profile 的固件格式/协议候选上可分离，
  但这不是对未注册物理板型的封闭世界证明；
- 同 SoC 家族第三方板在 maskrom/loader 下与 DAYU200 不可由 USB 区分——
  loader 起刷时 strength 最高到 `mode+device-info`，`model` 为 null。
  交互路径每次要求输入 profile 声明的型号全称；非交互路径若未同时
  给出显式 `--profile` 与精确 `--device`，拒绝
  `IDENTITY_CONFIRMATION_REQUIRED`。人工输入不会把 strength 提升为 strong；
  normal 模式起刷可经 HDC 读产品型号，strength 为 `strong`；
- DAYU600 的 download 模式 USB 身份未测（UNI-U05/U06 open），仅 normal 模式
  可识别，且识别后照常给出"仅支持检查，不支持执行"的 typed 结果。

### 2.3 intent

组合（profile × artifact）的合法 intent 集恰一时默认采用并在输出中回显
`"intent": {"value": "full-restore", "resolution": "defaulted"}`；
多于一个合法值时 intent 升级为必要信息（交互询问 / 非交互 refusal）。
`--intent` 始终可显式覆盖。

## 3. 命令定义

### 3.1 `status`（bare `arkforge` 等价）

~~~text
Effect: read-only（runtime 未运行时不自动拉起，如实报告 not-running）
~~~

单次调用聚合原 doctor + daemon status + device list + artifact list + job list：

~~~json
{
  "schema": "arkforge.status/v1",
  "captured_at_epoch_ms": 0,
  "complete": true,
  "host": {"platform_supported": true, "inspect_ready": true},
  "runtime": {"running": true, "pairing_epoch": 3, "mechanics_ready": true,
              "authority_support_available": false, "hdc_bound": true,
              "hardware_campaign": null, "execute_ready": false,
              "active_jobs": ["job:…"]},
  "devices": {"available": true, "complete": true, "items": [
    {"observation_id": "…", "mode": "rockusb-loader", "serial": null,
     "identity_strength": "…", "identification": {…}}
  ]},
  "artifacts": {"available": true, "complete": true, "items": [
    {"artifact_id": "…", "format": "rockchip-images-targz",
     "size_bytes": 0, "compatible_profiles": ["…"]}
  ]},
  "jobs": {"available": true, "complete": true, "items": [
    {"job_id": "…", "state": "running", "plan_id": "…"}
  ]},
  "blockers": [{"code": "…", "remediation": "…"}],
  "next_commands": ["…"]
}
~~~

区段不可观测不等于空集：

- runtime 未运行时，`devices`/`jobs` 为
  `{"available":false,"complete":false,"reason":"RUNTIME_NOT_RUNNING","items":null}`；
  本地 CAS 可读时 `artifacts` 仍是完整快照。
- runtime 已运行但某子查询失败时，该段 `available:false`、`items:null`，
  根文档 `complete:false`，并在 blockers 中保留 typed reason。
- 只有完成枚举且结果为零时才输出 `items:[]`。只要聚合文档成功产出就
  exit 0；主机根评估自身无法产出才 exit 10。

人类模式输出分节摘要 + 一条建议的下一步。

### 3.2 `flash run [FILE]`

~~~text
Usage:
  arkforge flash run [FILE]
    [--file <path> | --artifact <artifact-id>]
    [--target <selector> | --device <observation-id>]
    [--profile <id@version>] [--intent <intent>]
    [--hardware-campaign <id>]
    [--ack <token>]... [--wait-device <ms>] [--detach]

Effect: destructive（经确认屏或 --ack 门）
~~~

流水：runtime 确保 → 内容解析（FILE/--file 隐式 import，CAS 去重；--artifact
直接引用）→ 设备解析（§2.1）→ profile/intent 推断（§2.2/2.3）→ assessment →
plan 物化 → **同意门** → apply → 默认跟踪 job 至终态（`--detach` 保持现行语义，
Ctrl-C 停止跟踪不取消动作）。

同意门：

- 交互门开启：确认屏展示识别块（含证据与强度）、固件 hash、profile/intent
  及 resolution、全部 persistent effects、required_acknowledgements、当次
  HardwareCampaign（如有）。确认输入为 `y`；强物理身份下的
  (设备身份 × profile) 组合首次刷写，或 identification strength 低于 strong
  时，升级为要求输入 profile 声明的型号名全文。弱身份每次都升级，
  人工输入不改变 strength。确认即接受全部 required token。
- 交互门关闭：`--ack` 必须精确覆盖 required_acknowledgements；
  弱物理身份还必须显式给 `--profile` 与精确 `--device`，否则先拒绝
  `IDENTITY_CONFIRMATION_REQUIRED`；
  缺失 → `ACKNOWLEDGEMENT_REQUIRED`（信封 `facts` 内含完整 plan 摘要与
  直接执行该已 sealed plan 的顶层 `apply` 命令；不重跑 `flash run`、
  不静默物化第二个 plan。首跑不带 token 的失败本身就是 review）；
  多余 → `UNEXPECTED_ACKNOWLEDGEMENT`；effect 集漂移 → 同缺失处理。
- plan digest 在 run 内部同进程闭合校验；journal 与 receipt 不因入口不同而异。

在调用 supervisor apply 前，CLI authority 必须耐久写入 owner-only
`arkforge.cli-approval/v1` record，包含 plan ID/digest、精确 token 集、
`provenance: interactive-tty|argv`、人工输入的型号断言（如有）、campaign（如有）
与时间。写入失败时零 dispatch；对同一 approval ID 仅允许字节完全相同的
幂等重试。该 record 不修改 `arkforged` journal/receipt，不计入 mechanics evidence。

“首次刷写”仅对 strong physical identity 可缓存：key =
`physical_identity_digest × exact_profile_digest`，其中 physical identity 只取 model-binding
且跨观测稳定的事实，不取 observation-id/总线位置。只有 job 终态成功后
才记录该组合已成功；失败/中断不消耗首次确认。弱身份永不写入该缓存。

### 3.3 `flash plan`

~~~text
Usage:
  arkforge flash plan （入参同 flash run，另加 [--assess-only]，无 --ack/--detach）

Effect: read-only device access + host write（--assess-only 时无 plan 物化）
~~~

复合 staging：一次调用完成 import + 识别 + assess + 封 plan，输出单文档：

~~~json
{
  "schema": "arkforge.flash-plan/v2",
  "resolved": {
    "artifact": {"artifact_id": "…", "sha256": "…", "format": "…",
                 "manifest_summary": {…}, "imported": true},
    "device": {"observation_id": "…", "mode": "…", "identification": {…}},
    "profile": {"id": "…", "version": "…", "resolution": "inferred"},
    "intent": {"value": "full-restore", "resolution": "defaulted"}
  },
  "assessment": {"executable": true, "data_impact": […], "unknowns": […],
                  "blockers": []},
  "plan": {"plan_id": "…", "plan_sha256": "…", "ordered_steps": […],
            "persistent_effects": […], "required_acknowledgements": ["…"],
            "expires_at_epoch_ms": 0,
            "execution_context": {"mechanics_maturity": "…",
                                  "authority_support": "…",
                                  "hardware_campaign": null}},
  "apply_command": "arkforge apply --plan … --expect-plan-sha256 … --ack …",
  "next_commands": ["…"]
}
~~~

plan 若封入 HardwareCampaign，`apply_command`/`next_commands` 必须包含
`--hardware-campaign <sealed-id>`；不得依赖持久默认或当前 runtime 的隐式继承。

普通 `flash plan` 只在 plan 成功物化时 exit 0。门未通过时 exit 3，根文档为
`arkforge.command-result/v1` 错误信封，`code: PLAN_UNAVAILABLE`；
`error.facts.flash_plan` 内嵌同一 `arkforge.flash-plan/v2` 形状，其 `plan:null`、
`assessment.blockers` 非空、已完成阶段的 `resolved`/`assessment` 原样保留。
DAYU600 等不可执行结果也走此契约。

`--assess-only` 在成功产出 assessment 时始终 exit 0，即使 `executable:false`；
它输出根 `arkforge.flash-plan/v2` 文档且 `plan:null`，不物化 plan。
这保留原 `flash assess` 的“assessment 本身是答案”退出语义。

### 3.4 `apply`

~~~text
Usage:
  arkforge apply --plan <plan-id> --expect-plan-sha256 <sha256>
    [--ack <token>]... [--hardware-campaign <id>] [--detach]

Effect: destructive
~~~

语义与现行 `flash apply` 完全一致，提升为顶层通用同意动词：接受 normal
flash plan 与 `job recover` 产出的 recovery plan（两者均为 authority plan
域）。CLI 在读 store 前先按规范 ID 形状识别 `rescue-plan:<sha256>`，
拒绝并指向 `rescue apply`（救援域独立同意面保持）。
交互门开启且缺 `--ack` 时，允许以确认屏方式接受（同 §3.2）。
campaign plan 的 apply 命令必须包含已 sealed 的同一 campaign ID。
非交互时 `--ack` 仍为必填且必须精确覆盖；help 的 constraints 显式编码
这一条件必填关系。

### 3.5 `watch` / `cancel`

- `watch [--job <id>]`：缺省解析为最近一个非终态 job；无活动 job 时报告最近
  终态 job 摘要；多个活动 job → 交互选择 / 非交互 refusal 列候选。
  流式 JSONL 契约不变。
- `cancel --job <id> --expect-sequence <u64>`：语义不变（乐观并发、
  四态 disposition），仅位置提升。

### 3.6 `device list [--deep]` / `device wait`

- `list` 默认枚举 + 被动识别（固件无关的 usbIdentities 匹配与缓存的探针事实）；
  `--deep` 追加主动 probe（DEVICE_INFO / HDC 产品事实），逐台输出识别块。
  原 `device show`/`device probe` 并入（`--device` 过滤单台）。
- `wait` 语义不变；新增被 `flash run` 内部复用。

### 3.7 `artifact import / list / show`

- `import` 输出升级为复合文档：CAS 事实 + 解析 manifest + 兼容 profile 集 +
  **在场匹配设备**（runtime 运行时枚举求交，一次看出"这包固件能刷现在接着的
  哪台"）+ 下一步命令行。
- `show` 吸收原 `inspect` 的离线解析与 `--profile-file` 覆盖率检查。
- `list` 逐条携带 format 与 compatible_profiles。

### 3.8 `job show / reconcile / recover`

- `show` 内嵌：任务事实 + 事件尾部 + action receipts + `recovery` 块
  （eligible 与 typed guidance，原 `recovery guide` 并入）。
- `reconcile` 保留独立（主动设备核对是一次真实 I/O 决策）。
- `recover --job <id> [--target …]` 走与 `flash plan` 相同的推断引擎物化
  superseding plan，输出 `arkforge.flash-plan/v2` 文档，经顶层 `apply` 执行。
  no-replay 与新 epoch/intent 语义不变。

### 3.9 `config show / set / unset / add / remove`

可复用的本机工具绑定与开发 profile 默认项，owner-only 存储于
runtime-dir（Unix 用 owner mode，Windows 用等价用户 ACL；格式复用仓内已有
codec，不引第三方依赖）：

~~~text
arkforge config set hdc.path=/usr/local/bin/hdc hdc.sha256=<64hex>
arkforge config unset hdc
arkforge config add profile-file.path=/absolute/dev-profile.yaml \
  profile-file.sha256=<64hex>
arkforge config remove profile-file.sha256=<64hex>
arkforge config set daemon.require-release-signing=true
arkforge config unset daemon.require-release-signing
arkforge config show
~~~

配置规则：

- HDC path + digest 必须在一个原子 transaction 中设置/验证；拒绝相对路径。
- profile-file 在 `add` 时解析为规范绝对路径、复验期望 digest；
  每次启动前重新 hash，字节漂移则 typed refusal。`remove` 按 digest 精确删除。
- 更新通过同目录临时文件 + sync + atomic replace 提交；失败保留旧配置。
- `config show --output json` 为遵守 CLI-AC-04 不输出 host/HDC path，只返回
  binding 状态、digest 与 profile 数量；human 模式可向本机 owner 展示路径。
- `campaign` 不是合法 config 键；尝试设置返回 `CAMPAIGN_NOT_PERSISTABLE`
  并指向当次 `--hardware-campaign`。

`daemon run/start` 与自动确保读取同一配置；显式命令行参数只有在
完整提供一组精确值时才覆盖当次。HDC exact-digest 复验不变。

### 3.10 保持不变的命令

`rescue *`（刻意不做推断/交互流线化）、`daemon run/start/stop`、
`signing verify`、`completion` 行为不变。`daemon status` 并入顶层 `status`。

### 3.11 `help`

`help --all`（或 JSON 模式无路径）单次输出整棵树的
`arkforge.command-help-index/v1` 集合文档；Agent 一次调用获得全部契约：

~~~json
{
  "schema": "arkforge.command-help-index/v1",
  "command_count": 0,
  "commands": []
}
~~~

`commands` 按 path 字典序排列，每项是一份 `arkforge.command-help/v1` leaf。
原 leaf 字段含义不变，加性新增 `runtime_effect` 与 `facts_projections`；
effect 分级、constraints、examples、next_commands 契约保持。命令树 v1
总量有界，不分页；`command_count` 必须与数组长度相等。
`interactive:true` 只表示该 leaf 在 §1.1 交互门成立时可询问，不改变
JSON/JSONL/`--no-input` 的确定性。

## 4. 交互矩阵（验收基准）

| 场景 | TTY human | 非交互 |
|---|---|---|
| `flash run` 无内容参数 | 列表选择（CAS + cwd 已知格式） | refusal `CONTENT_REQUIRED` |
| 0 台候选设备 | waiting-for-device | refusal `DEVICE_NOT_FOUND`（`--wait-device` 可选） |
| 1 台候选设备 | 自动绑定，确认屏披露 | 自动绑定，输出披露 |
| N 台候选设备 | 编号选择器 | refusal `DEVICE_AMBIGUOUS` + 候选 facts |
| profile 兼容集恰一 + 强型号事实 | 自动采用，回显 resolution/strength | 同左 |
| profile 兼容集恰一 + 弱物理身份 | 每次输入型号全称，strength 不升级 | 缺显式 `--profile` + 精确 `--device` 时 `IDENTITY_CONFIRMATION_REQUIRED` |
| profile 交集空/多 | refusal 列各信号候选（不询问——非必要信息不问人） | 同左 |
| 缺同意 | 确认屏（首刷/弱身份升级为输入型号名） | refusal `ACKNOWLEDGEMENT_REQUIRED` + plan facts |
| token 多余/漂移 | 确认屏重新展示并要求重新确认 | `UNEXPECTED_ACKNOWLEDGEMENT` / 缺失处理 |
| runtime 未运行 | 自动拉起 + 提示行 | 自动拉起 + 文档标记（`--no-auto-start` 恢复 refusal） |
| campaign runtime | 当次必须显式给同一 ID | 同左；缺失/不同均 refusal |

表中 TTY human 始终指 stdin/stdout/stderr 均为 TTY；任一流重定向均按非交互列。

## 5. 实现约束

- 零第三方 Rust 运行时依赖红线不变：交互层 v1 只用行式编号提示
  （`Select device [1-3]:`），不进 raw mode、不做全屏 TUI、不做文件系统
  浏览器；内容列表 = CAS `artifact list` + cwd 一层已知格式 glob。
- 命令定义仍是单一 typed tree 生成 parser/human help/JSON help/completion；
  新增 leaf 与删除 leaf 遵守既有快照与 parse-only 测试纪律。
- config、approval record 与 strong-identity 首刷缓存均使用仓内 codec、
  owner-only 边界和 crash-safe atomic commit；三者分开 schema/store，不解读为 permit/
  mechanics journal/evidence。
- `flash run/plan` 的复合实现只允许调用既有 supervisor 原语
  （assess/materialize/apply/watch）与既有 client 查询，禁止旁路 IPC 面。
- 退出码表沿用 chg-agent-native-cli §9.3；新 refusal code 归入既有分类
  （3/5/6/4）。
