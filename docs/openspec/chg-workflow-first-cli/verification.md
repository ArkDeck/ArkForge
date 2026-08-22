# Verification — CHG-2026-CLI-WF

> Status: draft。所有新增验收均为软件面；不新增真机证据门，也不减免
> chg-agent-native-cli 既有 authority/permit/no-replay/rescue 不变量。CLI-AC-08..14、
> 17..32 的语义在新 canonical path 上复跑；CLI-AC-15（plan 拒绝文件路径）
> 被 WF-AC-10（先入 CAS 的复合 import）显式替代，CLI-AC-16 的 exact apply 门由
> WF-AC-13/20 覆盖。不得一边应用新 parser 一边照搬已被本 change 修订的旧命令形状。

## Acceptance matrix

| ID | Verification | Expected result |
|---|---|---|
| WF-AC-01 | 运行 `status`（runtime 运行/未运行/子查询失败三态） | 单文档聚合 host/runtime/devices/artifacts/jobs/blockers；未运行时 `devices/jobs.items:null` 而非空数组；子查询失败标 `complete:false`；真空枚举才是 `items:[]`；始终不自动拉起 |
| WF-AC-02 | `help --all` 与逐路径 `help` 输出对比 | 根为 `arkforge.command-help-index/v1`，`command_count` 等于按 path 字典序的 commands 数组长度；每个 leaf 与逐路径结果一致并含 `runtime_effect`/`facts_projections` |
| WF-AC-03 | 删除面扫描 | `doctor`、`daemon status` 不存在于 parser、help、completion 与打包物 |
| WF-AC-04 | `device list` 与 `--deep` 对比 | 默认仅被动识别；`--deep` 追加 DEVICE_INFO/HDC 事实；每台携带分离 compatible profile 与 physical model 的 identification 块；Loader 弱身份 `model:null` |
| WF-AC-05 | `artifact import` 复合输出 | 一次调用返回 CAS 事实 + manifest + 兼容 profile + 在场匹配设备；runtime 未运行时省略在场段并注明 |
| WF-AC-06 | `job show` 终态失败任务 | 内嵌事件尾、receipts 与 recovery 块；无需再调 recovery guide |
| WF-AC-07 | 设备解析矩阵（0/1/N × TTY/非交互 × 强/弱身份） | 按 design §4 逐格：唯一 observation 可自动绑定但不会自动提升物理型号；零台 waiting/refusal；多台选择/refusal；弱身份触发额外明示决策 |
| WF-AC-08 | profile 兼容与型号识别 | rockchip-targz + 0x2207 loader + `DEVICE_INFO Mode=Loader` 只得 compatible dayu200、`model:null`、strength ≤ `mode+device-info`；HDC productModel=DAYU200 才得 strong model；PAC 固件 → dayu600 且 assessment 不可执行；兼容交集空/多 → `PROFILE_AMBIGUOUS` |
| WF-AC-09 | `--target` 选择器 | 完整序列号、唯一前缀（≥4）、强事实得到的唯一 productModel 均解析至精确 observation；不得用唯一兼容 profile 伪造 productModel；歧义拒绝列候选；与 `--device` 互斥 |
| WF-AC-10 | `flash plan --file` | 字节先入 CAS（重复导入去重）、hash 封入 plan、输出报告 artifact_id；结构化模式位置参数被拒 |
| WF-AC-11 | `flash plan` 门未通过 | exit 3 + `arkforge.command-result/v1`/`PLAN_UNAVAILABLE`；`error.facts.flash_plan` 为 v2 文档且 `plan:null`、blockers 非空、resolved/assessment 保留 |
| WF-AC-12 | `--assess-only` | assessment 成功产出即 exit 0，即使 executable=false；根为 flash-plan/v2、`plan:null`，无 plan 物化副作用 |
| WF-AC-13 | 顶层 `apply` | 与原 `flash apply` 行为逐项一致（digest、token 精确覆盖、UNEXPECTED_ACKNOWLEDGEMENT、exit 码）；recovery plan 可执行；rescue ID 在读 authority store 前被拒并指向 `rescue apply`；campaign plan 缺少/错误当次 ID 零 dispatch |
| WF-AC-14 | `watch` 缺省解析 | 单活动 job 自动选中；无活动报告最近终态；多活动 TTY 选择/非交互 refusal |
| WF-AC-15 | 删除面扫描 | `flash assess`、`flash apply`、`job watch`、`job cancel`、`job recovery guide`、`device show/probe`、`artifact inspect` 均已不存在 |
| WF-AC-16 | 非交互永不提示 | stdin/stdout/stderr 各自单独非 TTY（含 `>file`、`2>file`、pipeline）、`--output json|jsonl`、`--no-input` 下对全部缺失信息场景验证进程不读 stdin，返回 typed refusal + facts |
| WF-AC-17 | `flash run` 交互全流程（脚本化 PTY） | 列表选内容 → 唯一设备自动绑定 → 确认屏含识别证据/强度/effects/tokens/campaign → 确认后执行至终态 |
| WF-AC-18 | 确认升级与首刷缓存 | strong identity 的精确 identity digest × profile digest 首次要求型号全文；只有成功终态后同组合才可降为 `y`；失败不消耗首次；弱身份每次都要求全文且 strength 不升级 |
| WF-AC-19 | 同意审计 | 独立 `arkforge.cli-approval/v1` 记录精确 plan/digest/tokens/campaign 与 `interactive-tty|argv` provenance；写入失败零 dispatch；同 ID 不同字节冲突；mechanics journal/receipt 字节与入口无关 |
| WF-AC-20 | 非交互 `--ack`/弱身份契约 | 弱身份缺显式 profile + 精确 device 先返 `IDENTITY_CONFIRMATION_REQUIRED`；缺 ack → `ACKNOWLEDGEMENT_REQUIRED` 且 facts 含已 sealed plan 与顶层 apply 命令，plan 数量不增长；多余 → `UNEXPECTED_ACKNOWLEDGEMENT`；effect 漂移不放行 |
| WF-AC-21 | run 内部 digest 闭合 | run 物化与执行之间 plan 被替换/过期时拒绝执行；journal 无第二次物化的静默复用 |
| WF-AC-22 | Ctrl-C 语义 | 确认屏前/中中断无 device mutation 且无 job/approval record；已完成的 CAS import/sealed plan 可保留为完整主机记录，无半写临时文件；执行中中断仅停止跟踪 |
| WF-AC-23 | `config show/set/unset/add/remove` | Unix owner mode/Windows 等价 ACL；HDC 成对原子绑定/清除；profile-file 只接受绝对路径 + digest 且漂移拒绝；注入各 durable boundary 失败均保留旧配置；`campaign` 键返 `CAMPAIGN_NOT_PERSISTABLE` |
| WF-AC-24 | runtime 自动确保 | 并发命令只启动一个 runtime；输出标记 autostart；`--no-auto-start` 恢复 refusal；ArkDeck 已配对无接管；campaign 缺失/不同返 mismatch 且不重启；help 对需要 runtime 的命令标 `runtime_effect:may-start-service` |
| WF-AC-25 | 结构化输出纪律回归 | 复合文档、config show 与 facts 信封均无 ANSI/进度文本/host 路径/HDC 路径/endpoint/秘密；每个 facts projection 不超过 help 声明上限（沿用 CLI-AC-04 扫描器） |
| WF-AC-26 | `job recover` | 复用推断引擎、输出 flash-plan/v2、新 epoch/intent；原 job 永不 resume；产物仅经顶层 `apply` 执行 |
| WF-AC-27 | 文档一致性 | 每个纵向提交均同步当次 README/architecture/调用方；最终 completion、human help、help-index 与命令树一致；command-help/v1 仅加性新增 `runtime_effect`/`facts_projections` |
