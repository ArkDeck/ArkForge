# ArkForge Specification v1

> 状态：`1.0.0-draft.3`（2026-08-23）。本目录是 ArkForge 的**语言无关规范正本**。
> Rust、Zig、C++、Swift 都只是这份规范的实现；Rust 工作区是第一个参考实现，
> 也是生成 conformance fixture 的 oracle，但**Rust 代码不是规范**。
>
> 中文导读见下一节；规范正文为英文，以便与 wire spelling、conformance case 和
> 代码符号一一对应。

## 导读（中文）

| 你要做什么 | 先读 |
|---|---|
| 用另一种语言移植 ArkForge | §3 Porting order，然后按阶段读对应 `requirements/` 切片与 `conformance/v1/` 套件 |
| 判断某个行为是否"必须保持" | `requirements/` 中带 `AF-…` ID 的条目；`status: normative` 的条目必须保持 |
| 校验自己的编码/摘要是否正确 | `conformance/v1/<suite>/<case>/case.json` + 同目录的完整字节文件 |
| 查某个字符串在线上怎么拼 | `model/vocabularies.yaml` |
| 查状态机与崩溃处置 | `state-machines/job.yaml`、`state-machines/crash-disposition.yaml` |
| 查 OS 边界（USB、fsync、时钟、IPC）该承诺什么 | `ports/` |
| 查稳定错误码 | `errors/registry.yaml` |
| 查一条要求对应哪个 Rust 符号与测试 | `mappings/rust.yaml` |
| 规范与代码/架构文档不一致的地方 | `ISSUES.md` |

本规范不复述 `docs/architecture.md`：那份文档保留为设计依据、ArkDeck 边界与历史；
本目录只保留可检查的约束。两者冲突时以本目录为准并在 `ISSUES.md` 登记
（见 `docs/decisions/AFD-0005-language-neutral-spec.md`）。

---

## 1. What this directory is

ArkForge is a device-independent firmware flashing engine. Its value is a set of
*semantics* — immutable plans, exact permits, a hash-chained durable journal,
never replaying an unknown outcome, three-state verification — and none of those
semantics depend on the language they are written in. This directory states them
once, in a form that any implementation can be checked against.

Everything under `spec/` is one of:

| kind | where | authority |
|---|---|---|
| **Conformance fixtures** — exact input/expected bytes | `conformance/v1/` | highest: bytes do not have opinions |
| **Machine-readable models** — CDDL, JSON Schema, YAML tables | `model/`, `state-machines/`, `errors/` | normative |
| **Requirements** — one checkable sentence per ID | `requirements/` | normative (per-item `status`) |
| **Port contracts** — what the OS boundary must promise | `ports/` | normative for the boundary, informative for the platform notes |
| **Mappings** — requirement → implementation symbol → test | `mappings/` | informative |
| **Issues** — known disagreements | `ISSUES.md` | informative |

Two files outside this directory are also normative and are referenced from
`manifest.yaml`: `proto/arkforge.proto` (the IPC wire schema) and the published
profiles under `profiles/` (data, loaded through the strict YAML subset).

## 2. Precedence

When two sources disagree, the higher one wins **and the disagreement is a spec
defect** to be fixed by a spec revision — never resolved by an implementation
quietly choosing a side:

1. `conformance/v1/` fixture bytes and `manifest.json`;
2. `model/`, `state-machines/`, `errors/` (machine-readable);
3. `requirements/` prose;
4. `docs/architecture.md` (design rationale; informative for this spec);
5. the Rust reference implementation — **not normative**. Where the spec is
   silent, the Rust behaviour is a *candidate* for the spec and must be filed in
   `ISSUES.md`, not copied.

`status:` on every file and requirement says how hard the statement is:
`normative` (MUST; backed by a conformance case), `draft` (extracted from the
reference implementation, believed correct, SHOULD; divergence must be reported),
`informative` (never a requirement). See `AUTHORING.md`.

## 3. Porting order

Every port proceeds in the same stages. A stage is complete when its
conformance suites pass in the new implementation **without calling the Rust
implementation**. Do not start a stage before the previous one is green; each
stage's spec slice is small enough to read whole.

| stage | deliverable | read | conformance suites |
|---:|---|---|---|
| 0 | SHA-256, HMAC-SHA-256, deterministic CBOR, Protobuf wire subset, strict YAML | `requirements/digest.md`, `requirements/ipc.md` §wire, `model/strict-yaml.md` | `sha256`, `hmac-sha256`, `canonical-cbor`, `protobuf`, `strict-yaml` |
| 1 | pure core: identifiers, effects, steps, plan, projection, permits, admission | `requirements/identifiers.md`, `requirements/plan.md`, `requirements/authority.md`, `model/digest-bodies.cddl`, `model/vocabularies.yaml` | `permit`, `admission`, `plan` |
| 2 | DeviceProfile loader, artifact parsers, manifest | `requirements/profile.md`, `requirements/artifact.md`, `model/profile.schema.json` | `plan` (cases 001–002) |
| 3 | journal, semantic action receipt, job state machine, every crash window | `requirements/engine.md`, `state-machines/` , `ports/durability.md` | `journal`, `action-receipt`, `crash`, `state-machine` |
| 4 | provider SPI, verification tri-state, rebind, reconciliation, transcript replay | `requirements/verification.md`, `requirements/recovery.md`, `requirements/transport.md`, `model/transcript.md`, `ports/transport-identity.md` | `rebind`, `reconcile`, `transcript-dispatch` |
| 5 | IPC framing, sessions, daemon API; CLI | `requirements/ipc.md`, `ports/ipc-framing.md`, `requirements/cli.md` | `protobuf` (framing cases), `cli` |
| 6 | OS transports: USB, local IPC endpoint, managed device control | `ports/usb.md`, `ports/device-control.md` | hardware-in-the-loop; see `docs/evidence/` |
| 7 | real-device campaign under a named `HardwareCampaign` | `docs/decisions/AFD-0004-hardware-campaign-maturity.md` | evidence ledger, not a fixture |

A full port implements the same CLI/IPC surface so it can be verified as a
black box across a process boundary (`arkforge --output json …`, the daemon
socket). Mixing languages inside one process is only recommended for the USB
leaf (`ports/usb.md`), through a C ABI that carries bytes and error classes and
nothing else.

## 4. Directory map

```text
spec/
├── README.md                  this file — entry point, precedence, porting order
├── AUTHORING.md               conventions for editing spec files
├── manifest.yaml              spec version, file inventory, status per file
├── glossary.md                one definition per term
├── ISSUES.md                  known disagreements between spec, code and architecture.md
├── requirements/              AF-<AREA>-<NNN> requirements, by area
├── model/                     CDDL digest bodies, vocabularies, strict YAML, schemas
├── state-machines/            job transitions, crash dispositions, permit ledger
├── ports/                     OS/hardware boundary contracts
├── errors/registry.yaml       stable codes per surface
├── conformance/v1/            fixtures (generated by crates/arkforge-conformance)
└── mappings/                  requirement → symbol → test, per implementation
```

## 5. How fixtures are produced and kept honest

`crates/arkforge-conformance` regenerates `conformance/v1/` from the Rust
reference implementation (`cargo run -p arkforge-conformance -- generate`). Its
integration test fails whenever the committed fixtures differ from what the
code produces today, so a behaviour change in Rust cannot land without a visible
fixture diff — which is reviewed as a **spec change** (bump `manifest.yaml`,
note it in `ISSUES.md` or the requirement it affects). Generation is
deterministic: no clocks, no randomness, no host paths.

A port is not expected to run that crate. It reads `conformance/v1/manifest.json`,
checks it holds the same files (SHA-256 per file), and runs its own runner over
each `case.json`.

## 6. What is deliberately out of scope

- ArkDeck-side policy (RuntimeCapability, operation catalog, lane ownership):
  `docs/architecture.md` §3, §7, §14.6.
- macOS code signing / Windows Authenticode and installer packaging: platform
  release engineering, `docs/decisions/AFD-0003-*.md`, `packaging/`.
- Hardware evidence and maturity publication: `docs/evidence/`, AFD-0004.
- The choice to vendor SHA-256/CBOR/DEFLATE in Rust (AFD-0001) is a *Rust
  implementation* decision. A port may use a library; what is normative is the
  behaviour the fixtures pin (no panic, bounded allocation, typed rejection,
  exact bytes). Record the choice in `mappings/<lang>.yaml`.
