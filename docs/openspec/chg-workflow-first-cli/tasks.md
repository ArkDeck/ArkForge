# Tasks — CHG-2026-CLI-WF

> Status: draft。全部任务受 unreleased migration 规矩约束：canonical handler、
> human/JSON help、测试与仓内调用方同车交付，被替换的旧 leaf 同提交删除，
> 无新旧命令名并存期。依赖链按纵向排列，每级只消费上一级已交付的面。

## TASK-WF-001 — `status` 聚合与 `help --all`

- Status: pending
- 顶层 `status`（bare `arkforge` 等价）聚合 host/runtime/devices/artifacts/jobs/
  blockers；`help --all` 单次输出全树 leaf 数组。
- 同提交删除 `doctor` 与 `daemon status`。
- 纯查询，无破坏面；先落以建立复合文档与 `facts` 错误信封的公共实现。
- Acceptance: WF-AC-01..03。

## TASK-WF-002 — 富化查询面

- Status: pending
- `device list [--deep]` 并入 show/probe；`artifact import` 复合输出（manifest +
  兼容 profile + 在场匹配设备）；`artifact show` 吸收离线 inspect；
  `job show` 内嵌事件尾/receipts/recovery guidance。
- 同提交删除 `device show`、`device probe`、`artifact inspect`、
  `job recovery guide`。
- Acceptance: WF-AC-04..06。

## TASK-WF-003 — 推断引擎与复合 `flash plan`

- Status: pending
- 设备解析（0/1/N 规则）、三信号 profile 推断、intent 默认、`--target` 选择器；
  `flash plan` 接受 `--file`（隐式 CAS import）并输出
  `arkforge.flash-plan/v2` 复合文档；`--assess-only` 覆盖原 assess。
- 同提交删除 `flash assess`；`flash plan` 旧参数形态直接替换。
- Acceptance: WF-AC-07..12。

## TASK-WF-004 — 顶层 `apply` / `watch` / `cancel`

- Status: pending
- `apply` 提升为通用同意动词（normal + recovery plan，拒绝 rescue plan）；
  `watch` 默认最近活动 job；`cancel` 位置提升，语义不变。
- 同提交删除 `flash apply`、`job watch`、`job cancel`。
- Acceptance: WF-AC-13..15。

## TASK-WF-005 — `flash run` 与交互层 v1

- Status: pending
- 一步动词：runtime 确保 → 内容/设备/profile/intent 解析 → assessment →
  plan → 同意门 → apply → 跟踪；交互门规则（TTY ∧ human ∧ 无 --no-input）；
  行式编号选择器、确认屏（含识别证据与强度、首刷/弱身份升级）、
  waiting-for-device；非交互 `--ack` 契约与 `facts` 信封。
- Acceptance: WF-AC-16..22。

## TASK-WF-006 — `config` 与 runtime 自动确保

- Status: pending
- `config show/set`（HDC 绑定、campaign、profile-file；owner-only 存储，
  零第三方依赖 codec）；需要 runtime 的命令自动拉起并读取 config；
  `--no-auto-start` 退出；`AUTHORITY_ALREADY_PAIRED` 语义复验。
- Acceptance: WF-AC-23..25。

## TASK-WF-007 — `job recover` 与文档同步

- Status: pending
- `job recover` 复用推断引擎物化 superseding plan（输出 flash-plan/v2，
  经顶层 `apply` 执行）；同提交删除 `job recovery plan`。
- README、architecture.md 命令面章节、completion 与 help 快照全部同步。
- Acceptance: WF-AC-26..27。
