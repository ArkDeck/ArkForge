# Tasks — CHG-2026-CLI

> Status: software-complete by explicit maintainer request. Real-device campaign,
> maintainer review and production support publication remain external evidence gates.

## TASK-CLI-001 — One typed command tree

- Status: software-complete
- Build a new `arkforge` binary from one typed command definition.
- Generate parser metadata, human help, JSON help and shell completions from the
  same tree.
- Implement global output, error envelope and exit-code contracts.
- Add parse-only tests for every example and every invalid option relation.
- Acceptance: CLI-AC-01..04.

Progress:

- [x] semantic artifact/profile/observation/job/plan/token validators reject paths,
  placeholders and ellipses before I/O;
- [x] all success documents use `schema`; JSON/JSONL redact host/control secrets and
  provide actionable `next_commands` without echoing runtime paths;
- [x] macOS packaging contains only the signed sibling `arkforge`/`arkforged` pair.

## TASK-CLI-002 — Consolidate current read-only commands

- Status: complete
- Move public-socket discover/inspect/assess/job queries behind the new handlers.
- Move offline import/inspect and signing verification behind the new handlers.
- Add missing read APIs: probe, watch, reconcile and recovery guide.
- Migrate in closed verticals: canonical handler, human/JSON help, tests, packaging
  callers, then delete that old binary in the same commit. The first vertical is
  `signing verify`, followed by public-socket queries, then artifact import/inspect.
- ArkForge is unreleased, so no wrappers, deprecated syntax or dual command names
  are retained.
- Acceptance: CLI-AC-05..07.

Progress:

- [x] `signing verify`: canonical human/JSON handler, package-script caller,
  legacy binary removal;
- [x] public-socket device/artifact/flash-assessment/job queries，含 probe 与
  recovery guide；legacy `arkforge-cli` 已移除；
- [x] offline artifact import/inspect/list，profile coverage 对照与 legacy
  `arkforge-inspect` removal；
- [x] `device show/wait` 与 public `job watch`，包含 exact identity、resume
  sequence、timeout 和 ambiguity refusal；
- [x] controller `job reconcile` 与 distinct superseding recovery plan handler。

## TASK-CLI-003 — Direct CLI authority

- Status: software-complete; real-device campaign pending
- Add an authority-side crate/boundary that can mint exact StepPermit bytes;
  keep all minting symbols unreachable from `arkforged`.
- Implement a persistent local authority supervisor, dedicated runtime pairing,
  local target binding, permit durability, event watching and typed managed-control
  receipts. Pairing secret remains in supervisor/daemon memory.
- Implement the HDC control port for enter Loader, exact rebind and build
  postflight without leaking paths/endpoints/connect keys/argv.
- Add the independent `AuthoritySupportKey` gate and seal it into executable plans;
  do not overload the existing mechanics `evidence_set_digest`.
- Implement `--detach` only as a presentation choice; the supervisor remains the
  authority. Define command disconnect, explicit cancel, same-epoch retransmit and
  supervisor/daemon epoch-rotation behavior.
- Acceptance: CLI-AC-08..14.

Progress:

- [x] canonical `arkforge` 迁入独立 `arkforge-cli` crate，daemon 源码继续由
  architecture guard 禁止引用 authority-side minting；
- [x] `daemon run/start/status/stop` 共用持久 supervisor；pairing secret 仅经匿名
  stdin pipe 进入 daemon，owner-only socket 不暴露 secret，active job 禁止 stop；
- [x] shipped profiles 默认加载，`--profile-file` 只能附加且同一 `id@version`
  不可覆盖；CLI/IPC profile vocabulary 统一为 `id@version`；
- [x] durable target binding、permit loop、typed managed HDC、独立
  `AuthoritySupportKey` 双门、authority liveness pipe、持久单调 epoch、同 epoch
  exact retransmit 与 restart/no-replay matrix；apply 会重新核对当前 CLI/HDC/campaign
  support key 与 daemon toolchain，任一重建或替换都必须重新 plan。

## TASK-CLI-004 — Normal `flash plan/apply`

- Status: software-complete; real-device execution evidence pending
- Implement exact artifact/profile/device/intent materialization.
- Derive acknowledgement tokens from the sealed effect set.
- Require plan digest and the exact token set at apply.
- Render ordered events and terminal outcome in human, JSON and JSONL modes.
- Preserve no-replay classification across CLI/daemon death.
- Acceptance: CLI-AC-15..19.

Progress:

- [x] exact artifact/profile/device/intent materialization and unique sealed plan IDs;
- [x] exact plan digest plus complete acknowledgement-set equality before job creation;
- [x] persistent supervisor permit/control loop, `--detach`, safe-boundary cancel,
  reconcile/recovery, human/JSON and ordered metadata→event→terminal JSONL;
- [x] daemon restart rehydrates pre-intent work as cancelled-safe and unsettled
  intent as outcome-unknown with no replay; supervisor restart rotates the durable epoch.

## TASK-CLI-005 — Separate native RockUSB rescue domain

- Status: software-complete; real-device campaign and release evidence remain pending
- Add `RescuePlan`, `RescueReceipt`, a separate store namespace and a closed
  native rescue executor that reuses `NativeRockUsbPort`/`RockUsbProtocol`.
- Seal the running ArkForge build digest, exact USB observation, profile, layout
  and image facts into each plan.
- Implement semantic read commands and two-phase write/reset.
- Forbid external device tools, device-tool subprocesses, arbitrary USB requests,
  raw LBA write and automatic normal fallback.
- Ensure normal receipt/maturity readers cannot decode rescue evidence.
- Acceptance: CLI-AC-20..27.

## TASK-CLI-006 — Hardware campaign and release gate

- Status: externally blocked; software campaign entry and evidence contracts complete
- Run the full CLI-authority DAYU200 campaign: normal-to-Loader, nine writes,
  read-domain-aware verification, reset, exact HDC postflight and receipts.
- Run a controlled native rescue campaign for each closed action.
- Run CLI/daemon crash, cable detach, multi-device ambiguity and cancel cases.
- Publish only the exact CLI authority support key against the already exact
  mechanics maturity key after maintainer review. Rescue remains separately classified.
- Acceptance: CLI-AC-28..32.

Progress:

- [x] `daemon run/start --hardware-campaign <id>` opens only named mechanics and
  CLI-authority `HardwareCampaign` states and seals both into the plan;
- [x] campaign state is reported by status/help and never populates the production registry;
- [x] production `AuthoritySupportKey` registry intentionally remains empty;
- [ ] execute CLI-AC-28..31 on controlled DAYU200 hardware and attach evidence;
- [ ] maintainer review and exact-key production publication for CLI-AC-32.

## Delivery order

~~~text
CLI-001 typed tree
  └─ CLI-002 read-only consolidation
       ├─ CLI-003 direct authority ── CLI-004 normal flash ──┐
       └─ CLI-005 explicit rescue ──────────────────────────┤
                                                            └─ CLI-006 hardware release gate
~~~

Normal flash and rescue may be implemented in parallel after the read-only
surface lands, but neither may be called supported before its own hardware and
negative-path acceptance is recorded.

## Old binary removal

Delete each of `arkforge-cli`, `arkforge-inspect` and `arkforge-signing` in the
same vertical that lands all of that binary's canonical `arkforge` handlers and
updates its in-repository callers. There is no release compatibility gate because
ArkForge has not shipped.
