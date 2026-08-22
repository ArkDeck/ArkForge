# Verification — CHG-2026-CLI-WF

> Status: draft。所有验收均为软件面；不新增真机证据门，也不减免
> chg-agent-native-cli 既有的 CLI-AC-08..32（authority/permit/rescue 语义
> 未被本 change 触碰，其验收原样有效并须在每级纵向交付后复跑）。

## Acceptance matrix

| ID | Verification | Expected result |
|---|---|---|
| WF-AC-01 | 运行 `status`（runtime 运行/未运行两态） | 单文档聚合 host/runtime/devices/artifacts/jobs/blockers；未运行时如实 `running:false` 且不自动拉起 |
| WF-AC-02 | `help --all` 与逐路径 `help` 输出对比 | 全树 leaf 数组与逐路径结果逐字段一致；单次调用覆盖所有 leaf |
| WF-AC-03 | 删除面扫描 | `doctor`、`daemon status` 不存在于 parser、help、completion 与打包物 |
| WF-AC-04 | `device list` 与 `--deep` 对比 | 默认仅被动识别；`--deep` 追加 DEVICE_INFO/HDC 事实；每台携带 identification 块（model/evidence/strength） |
| WF-AC-05 | `artifact import` 复合输出 | 一次调用返回 CAS 事实 + manifest + 兼容 profile + 在场匹配设备；runtime 未运行时省略在场段并注明 |
| WF-AC-06 | `job show` 终态失败任务 | 内嵌事件尾、receipts 与 recovery 块；无需再调 recovery guide |
| WF-AC-07 | 设备解析矩阵（0/1/N × TTY/非交互） | 按 design §4 表逐格：唯一自动绑定并披露、零台 waiting/refusal、多台选择器/`DEVICE_AMBIGUOUS` + 候选 facts |
| WF-AC-08 | 三信号 profile 推断 | rockchip-targz + 0x2207 loader + DEVICE_INFO → dayu200 恰一；PAC 固件 → dayu600 且 assessment 报不可执行；交集空/多 → `PROFILE_AMBIGUOUS` 列各信号候选 |
| WF-AC-09 | `--target` 选择器 | 完整序列号、唯一前缀（≥4）、唯一在场型号名均解析至精确 observation；歧义拒绝列候选；与 `--device` 互斥 |
| WF-AC-10 | `flash plan --file` | 字节先入 CAS（重复导入去重）、hash 封入 plan、输出报告 artifact_id；结构化模式位置参数被拒 |
| WF-AC-11 | `flash plan` 门未通过 | 同一 v2 文档形状，`plan:null`、blockers 非空、resolved/assessment 保留 |
| WF-AC-12 | `--assess-only` | 无 plan 物化副作用；输出与门未通过分支的 assessment 段一致 |
| WF-AC-13 | 顶层 `apply` | 与原 `flash apply` 行为逐项一致（digest、token 精确覆盖、UNEXPECTED_ACKNOWLEDGEMENT、exit 码）；recovery plan 可执行；rescue plan 被拒并指向 `rescue apply` |
| WF-AC-14 | `watch` 缺省解析 | 单活动 job 自动选中；无活动报告最近终态；多活动 TTY 选择/非交互 refusal |
| WF-AC-15 | 删除面扫描 | `flash assess`、`flash apply`、`job watch`、`job cancel`、`job recovery guide`、`device show/probe`、`artifact inspect` 均已不存在 |
| WF-AC-16 | 非交互永不提示 | stdin 非 TTY、`--output json|jsonl`、`--no-input` 三种情形下对全部命令注入缺失信息场景：进程不读 stdin，返回 typed refusal + `facts` |
| WF-AC-17 | `flash run` 交互全流程（脚本化 PTY） | 列表选内容 → 唯一设备自动绑定 → 确认屏含识别证据/强度/effects/tokens → `y` 后执行至终态 |
| WF-AC-18 | 确认升级规则 | 首刷（设备身份 × profile）组合或 strength < strong 时要求输入型号名全文；错误输入不执行 |
| WF-AC-19 | 交互签发审计 | journal 中 acknowledgement 记录 `provenance: interactive-tty`；`--ack` 路径记录 `argv`；receipt 与入口无关 |
| WF-AC-20 | 非交互 `--ack` 契约 | 缺失 → `ACKNOWLEDGEMENT_REQUIRED` 且 `facts` 含 plan 摘要与完整重跑命令行；多余 → `UNEXPECTED_ACKNOWLEDGEMENT`；effect 漂移 → 重新要求，旧 token 集不放行 |
| WF-AC-21 | run 内部 digest 闭合 | run 物化与执行之间 plan 被替换/过期时拒绝执行；journal 无第二次物化的静默复用 |
| WF-AC-22 | Ctrl-C 语义 | 确认屏前中断零副作用；执行中中断仅停止跟踪，job 继续由 supervisor 驱动 |
| WF-AC-23 | `config set/show` | owner-only 权限；HDC 每次调用前 exact-digest 复验不变；campaign 仅产生 campaign 证据的语义不变 |
| WF-AC-24 | runtime 自动确保 | 未运行时自动拉起并在输出标记；`--no-auto-start` 恢复 refusal；ArkDeck 已配对 runtime 返回 `AUTHORITY_ALREADY_PAIRED` 且无接管路径 |
| WF-AC-25 | 结构化输出纪律回归 | 复合文档与 `facts` 信封均无 ANSI/进度文本/host 路径/HDC 路径/endpoint/秘密（沿用 CLI-AC-04 扫描器） |
| WF-AC-26 | `job recover` | 复用推断引擎、输出 flash-plan/v2、新 epoch/intent；原 job 永不 resume；产物仅经顶层 `apply` 执行 |
| WF-AC-27 | 文档一致性 | README/architecture 命令章节、completion、human/JSON help 快照与最终树一致；`arkforge.command-help/v1` 逐 leaf 结构未变 |
