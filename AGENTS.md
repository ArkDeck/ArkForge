# ArkForge Agent Guide

ArkForge 是设备无关的固件操作平台：Rust workspace 实现 CLI、daemon、固件解析、协议与
传输，Swift package 提供客户端。目标是完成用户当前任务，用与改动相称的证据验证结果。

## 工作方式

- 明确目标行为与完成条件后，直接完成实现、相关验证与最小文档更新。常规实现选择自行
  判断；只有缺失信息会实质改变范围、正确性或未获授权的副作用时才提问，同时继续独立工作。
- 先定位相关代码和测试，再按下表读取所需规范；不通读架构文档、历史任务或所有 skills。
  保留用户已有改动，不把无关重构、治理整理或跨仓修改加入当前任务。
- 沿用已有授权；仓库修改与真实设备执行是不同授权范围。编写和测试修复不意味着可以
  刷写设备、重置设备或发布硬件支持。
- 检查通过且完成条件满足后交付。仅因新改动、失败或未解决风险扩大或重复检查。
  使用用户的语言，简要报告实际修改、验证及未完成项，区分实测、推断和未验证结论。

## 按任务定位

| 任务 | 入口 |
| --- | --- |
| 构建与用法 | [README.md](README.md)、`Cargo.toml`、`rust-toolchain.toml`；实际 flags 查当前 CLI help |
| CLI 与独立授权 | `crates/arkforge-cli/`、`crates/arkforge-client/`、`crates/arkforge-standalone/`、`spec/requirements/cli.md` |
| 计划、摘要、permit | `crates/arkforge-core/`、`crates/arkforge-authority-api/`；对应 `spec/requirements/` 与 `spec/model/` |
| 固件、Profile、Provider、传输 | `crates/arkforge-artifact/`、`crates/arkforge-provider/`、`crates/arkforge-transport/`、`crates/arkforge-usb/`、`profiles/` |
| Job、journal、恢复、daemon | `crates/arkforge-engine/`、`crates/arkforged/`；`spec/state-machines/`、`spec/ports/` |
| IPC 与 Swift SDK | `proto/arkforge.proto`、`crates/arkforge-ipc/`、根目录 `Package.swift`、`swift/ArkForgeSDK/` |
| ArkDeck 集成 | `adapters/arkforge-arkdeck-adapter/`；实际修改 ArkDeck 时读取该仓 `AGENTS.md` |
| 打包、硬件状态与证据 | `packaging/`、[TASKS.md](TASKS.md)、[evidence ledger](docs/evidence/ledger.md)；按需读取相关 `docs/decisions/` |

## 规范与实现边界

- [spec/README.md](spec/README.md) 定义规范权威顺序：conformance fixture bytes >
  machine-readable models/state-machines/errors > requirements > 架构文档 > Rust 实现。
  同时检查 normative/draft/informative 状态。代码不是规范；差异登记到 `spec/ISSUES.md`，
  不靠改 expected 值掩盖行为变化。
- 修改规范、wire、摘要或生成 fixture 时读取 [spec/AUTHORING.md](spec/AUTHORING.md)，
  同步实际受影响的 model、requirement、manifest、mapping 与 conformance case。
  fixture 变化按规范变更审阅，不能用重新生成代替正确性判断。
- Rust workspace 按 [AFD-0001](docs/decisions/AFD-0001-zero-dependency-core.md) 保持零第三方
  运行时依赖。复用仓内原语与公开向量；工具链版本以 `rust-toolchain.toml` 为准。
- ArkForge mechanics 执行已授权计划；独立 CLI 与 ArkDeck 各自承担 authority，使用独立
  runtime namespace。Core 不引入 ArkDeck 专用类型，不接管另一 authority 的 daemon。
- `docs/architecture.md` 与 `docs/openspec/` 含历史设计，先检查 superseded 注记。
  现行 DAYU200 执行层是原生 RockUSB；不照抄退役 vendor-tool 启动契约或旧 CLI。

## 设备执行与证据

- 设备操作使用当前 `arkforge` typed CLI/client 与 daemon 路径。计划绑定精确设备
  observation、artifact/profile/toolchain、effects 与 authority；不绕过准入直调 vendor 工具。
- 独立 CLI 的 apply 匹配封存 plan digest 与完整 acknowledgement 集合，不增加宽泛
  `--yes`/`--force`；ArkDeck 集成遵循对应 authority/permit 契约。
  `--hardware-campaign` 仅用于已授权的具名硬件验收，不豁免安全门或自动发布生产支持。
- 副作用前持久化 intent；未知 outcome 永不自动 replay。不以退出码、相似设备或历史
  observation 推断成功。独立 recovery/rescue 遵循自身计划、授权与证据域。
- 设备支持与成熟度按当前 registry、Profile 和 evidence ledger 判断，不把任务完成、
  fake、transcript replay 或 plan-only 记为真机验收。DAYU600 的执行能力以证据门为准，
  不从 PAC 解析或非可执行 PlanAssessment 推导可刷写。

## 验证入口

命令在仓库根目录执行。开发反馈可先用 `cargo test -p <crate> <test-filter>`；
修改 Rust 运行时或跨 crate 行为时，交付前完成 CI 对应检查：

```bash
cargo fmt --all -- --check
cargo clippy --workspace --all-targets --all-features -- -D warnings
cargo test --workspace --all-targets
```

- 纯说明文字改动核对路径、链接、命令与 diff；不为此编写重复措辞的测试或执行设备验收。
- 修改 Swift SDK 或 Rust/Swift wire 兼容行为时，从根目录执行 `swift test`，并验证受影响的
  Rust crate；Swift package manifest 不在 SDK 子目录。
- 规范或编码变更运行 `cargo test -p arkforge-conformance`。仅在预期更新 fixture 时运行
  `cargo run -p arkforge-conformance -- generate`，并审阅生成 diff。
- Windows 与打包验证按 `.github/workflows/windows.yml` 和 `packaging/windows/README.md`
  执行；本机 macOS 通过不能证明 Windows 行为或硬件支持。
- 环境阻塞时保留具体错误，完成其余检查，明确未验证范围；不把环境问题报成通过。
  交付前检查 `git diff --check` 和实际 diff。

## Skills 与指令维护

使用用户指定或任务所需的 skill，读取 `SKILL.md` 后仅加载相关 references。用户明确要求
优先于 skill 流程建议；若指令导致暂停，链接具体文件、引用条款并说明缺口，区分要求与自身
解释。项目 skill 放在 `.agents/skills/<name>/SKILL.md`，明确触发条件、输入与产出，只存放
专门知识，不复制通用规则、固定“最新”模型版本或任务状态。删除前检查引用及脚本用途，
保持入口可发现；个人与插件 skills 属于独立修改范围。

维护依据：[OpenAI 提示指导](https://developers.openai.com/api/docs/guides/latest-model#prompting-best-practices)、
[AGENTS.md](https://learn.chatgpt.com/docs/agent-configuration/agents-md)、
[Skills](https://learn.chatgpt.com/docs/build-skills)。仅在维护指令或遇到相关问题时查阅。
