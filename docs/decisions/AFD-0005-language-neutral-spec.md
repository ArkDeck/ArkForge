# AFD-0005 — 语言无关规范：`spec/` 是正本，Rust 是第一个实现

- 状态：Accepted
- 日期：2026-08-23
- 关联：architecture.md 全篇；AFD-0001；`spec/README.md`；`spec/ISSUES.md`

## 背景：ArkForge 的价值不依赖语言

ArkForge 不是 UI 工程。它的价值是一组**语义**——不可变计划、精确的一次性 permit、
哈希链耐久 journal、`outcomeUnknown` 永不 replay、三态验证、读域实测——
这些语义没有一条依赖 Rust。但到目前为止它们散落在三处，且互相不完全一致：

- `docs/architecture.md`（2 300 多行，规范、Rust 示例、历史与实施状态混写）；
- Rust 代码（六万行，才是真正在真机上跑过的行为）；
- 一份只钉摘要不钉字节的 permit 向量文档，[它自己承认](../openspec/chg-arkdeck-arkforge-authority/permit-vectors.md)对不上时得运行 Rust 才能诊断。

一个 Agent 要用 Zig 或 C++ 做移植，面对的是"理解并翻译六万行 Rust"。而 Swift SDK
（`swift/ArkForgeSDK`）其实已经是 IPC 与 permit 的第二个实现——它的正确性靠几组
内联十六进制向量维持。这证明了两件事：多语言实现是现实需求，而现有的跨语言契约
太薄。

## 决定

1. 新建 `spec/`，它是 ArkForge 的**语言无关规范正本**。优先级：conformance fixture
   字节 > 机器可读模型/状态表/错误注册表 > 要求条文 > `docs/architecture.md`（设计
   依据）> Rust 实现（**不是规范**）。冲突是规范缺陷，在 `spec/ISSUES.md` 登记，
   由维护者决定，不允许任何实现悄悄选边。
2. 每条要求有稳定 ID（`AF-<AREA>-<NNN>`），说明 `status`（normative / draft /
   informative）、依据、对应 conformance case 与实现符号。
3. `crates/arkforge-conformance` 用 Rust 作为 oracle **生成**完整字节级 fixture
   （SHA-256、HMAC、canonical CBOR、permit、admission、journal 撕裂尾部穷举、
   crash disposition、状态机边集、Protobuf、rebind、严格 YAML、DAYU200 全链路
   plan lowering），生成后提交并 review；其集成测试在 Rust 行为漂移时失败，
   把"改了编码"变成一次可见的规范修订。第二实现靠复现这些字节通过，不调用 Rust。
4. Port 按固定阶段推进（`spec/README.md` §3），每阶段只读一个规范切片、通过对应
   套件后再进入下一阶段。完整 port 实现同一 CLI/IPC，以进程边界黑盒验证；
   只有 USB 叶子允许通过 C ABI 混合链接。
5. `docs/architecture.md` 保留为设计依据、ArkDeck 边界与历史；它的状态图、
   Rust 示例不再是任何语义的正本。

## 提取时发现的偏差（摘要，详见 `spec/ISSUES.md`）

- §13.1 状态图缺五条代码中存在的合法边（SI-001）；
- journal 同一批记录种类有两个写入者（engine 的 typed helper 与 daemon 自己），
  daemon 不写 `externalDispatchStarted`、部分记录不带 `jobId`（SI-002/012）；
- engine 的 `CrashDisposition::derive` 与 daemon 的 `recover_job` 对"intent 已落、
  尚未 consuming"给出不同结论（outcomeUnknown vs cancelledSafe），且 daemon 并不
  调用前者；daemon 重启后从不续跑任何 job（SI-003）；
- `cancel` 在 `preflight` 直接赋值状态绕过合法性检查（SI-004）；
- `readOnlyDispatch`/`rebindWait`/`reconciling` 三个状态 daemon 从未进入（SI-005）；
- YAML 子集对值内的 `&`/`*`/`!` 并不拒绝，与模块注释相反（SI-010）。

这些历史偏差已在 `1.0.0-draft.2`/`draft.3` 依次关闭。当前正本、Rust
实现、架构状态图和 conformance fixture 对这些语义给出同一答案；逐项决定与回归入口
保留在 `spec/ISSUES.md`。

## 不做什么

- 不移动 Rust 代码、不改 crate 边界；
- 不把 AFD-0001 的零依赖决定写进规范——那是 Rust 实现的选择，port 可用库，
  但必须通过同样的 fixture；
- 不删除历史 issue；关闭项仍保留决定、实现位置与回归证据。

## 复审条件

- 第一个非 Rust 实现通过 stage 0–3 套件时，把那些条目从 `draft` 升为 `normative`；
- `spec/ISSUES.md` 任一条拍板后，同一次变更里更新要求条文、fixture 与 `manifest.yaml`。
