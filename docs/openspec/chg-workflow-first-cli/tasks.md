# Tasks — CHG-2026-CLI-WF

> Status: draft。全部任务受 unreleased migration 规矩约束：canonical handler、
> human/JSON help、测试、仓内调用方与该命令的 README/architecture 文档
> 同车交付，被替换的旧 leaf 同提交删除，无新旧命令名并存期。
> 依赖链按下列纵向顺序，每级只消费上方已交付的面。

## TASK-WF-001 — `status` 聚合与 `help --all`

- Status: done
- 顶层 `status`（bare `arkforge` 等价）聚合 host/runtime/devices/artifacts/jobs/
  blockers，对 unknown/empty/partial 作显式区分；`help --all` 输出
  `arkforge.command-help-index/v1`。
- 同提交删除 `doctor` 与 `daemon status`。
- 纯查询，无破坏面；先落以建立复合文档与 `facts` 错误信封的公共实现。
- 同提交更新 README/architecture 根命令与输出 schema 章节。
- Acceptance: WF-AC-01..03。

## TASK-WF-002 — 富化查询面

- Status: done
- `device list [--deep]` 并入 show/probe；`artifact import` 复合输出（manifest +
  兼容 profile + 在场匹配设备）；`artifact show` 吸收离线 inspect；
  `job show` 内嵌事件尾/receipts/recovery guidance。
- 同提交删除 `device show`、`device probe`、`artifact inspect`、
  `job recovery guide`。
- 同提交更新受影响的 README/architecture 查询面。
- Acceptance: WF-AC-04..06。

## TASK-WF-003 — 推断引擎与复合 `flash plan`

- Status: pending
- 设备解析（0/1/N 规则）、profile 兼容与物理型号分层推断、intent 默认、
  `--target` 选择器；弱 Loader 身份在非交互下必须显式 `--profile` + 精确
  `--device`，人工断言不提升 strength。
  `flash plan` 接受 `--file`（隐式 CAS import）并输出
  `arkforge.flash-plan/v2` 复合文档；plan blocker 为 exit 3 + facts，
  `--assess-only` 保留 exit 0 语义。campaign 只接受当次显式参数。
- 同提交删除 `flash assess`；`flash plan` 旧参数形态直接替换。
- 同提交更新 README/architecture 的 normal planning 路径。
- Acceptance: WF-AC-07..12。

## TASK-WF-004 — 顶层 `apply` / `watch` / `cancel`

- Status: pending
- `apply` 提升为通用同意动词（normal + recovery plan，拒绝 rescue plan）；
  `watch` 默认最近活动 job；`cancel` 位置提升，语义不变。
- campaign plan 的 `apply` 要求当次显式给相同 ID；此阶段 `apply` 仍仅支持
  argv acknowledgement，交互确认在 TASK-WF-006 落地。
- 同提交删除 `flash apply`、`job watch`、`job cancel`。
- 同提交更新 README/architecture 的 apply/job 路径。
- Acceptance: WF-AC-13..15。

## TASK-WF-005 — `config` 与 runtime 自动确保

- Status: pending
- `config show/set/unset/add/remove`（HDC 绑定、profile-file、release-signing；
  owner-only 存储、绝对路径 + digest、原子更新）；`campaign` 键显式拒绝。
- 需要 runtime 的命令在 owner-only 启动锁下并发幂等自动拉起，读取 config；
  `--no-auto-start` 退出；`AUTHORITY_ALREADY_PAIRED`、campaign mismatch 与
  `runtime_effect` help 语义复验。
- 同提交更新 README/architecture 的 runtime/config 章节。
- Acceptance: WF-AC-23..25。

## TASK-WF-006 — `flash run` 与交互层 v1

- Status: pending
- 一步动词：runtime 确保 → 内容/设备/profile/intent 解析 → assessment →
  plan → 同意门 → apply → 跟踪；交互门规则（stdin/stdout/stderr 均为 TTY
  ∧ human ∧ 无 --no-input）；行式编号选择器、确认屏、waiting-for-device。
- 强身份首刷缓存仅在成功终态后记录；弱身份每次输入型号全称。
  非交互缺 ack 时返回已 sealed plan 的顶层 apply 命令，不重物化。
- 新增独立 `arkforge.cli-approval/v1` authority audit record；耐久写入失败时
  零 dispatch，不修改 mechanics journal/receipt。
- 同提交更新 README/architecture 的交互与审批记录章节。
- Acceptance: WF-AC-16..22。

## TASK-WF-007 — `job recover` 与文档同步

- Status: pending
- `job recover` 复用推断引擎物化 superseding plan（输出 flash-plan/v2，
  经顶层 `apply` 执行）；同提交删除 `job recovery plan`。
- 同提交更新 recovery 的 README/architecture 章节，并做最终全树一致性扫描：
  completion、human/JSON help 快照与所有前序文档必须已在各自纵向交付中同步。
- Acceptance: WF-AC-26..27。
