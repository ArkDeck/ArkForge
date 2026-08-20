# OpenSpec change 草案

本目录保存尚待维护者 review 的跨边界 change。草案不等于批准，更不开放新的
设备写入能力。当前既包含要复制进 ArkDeck 治理流程的跨仓提案，也包含 ArkForge
本仓能力设计；以各 change 的 front matter 和 proposal 边界为准。

跨仓提案放在这里而不是直接开 ArkDeck 的 PR，是因为提案的**依据**在本仓：
真机实测、读域证据、permit 编码。提案与依据不该分居两地。

| 目录 | 内容 | 状态 |
|---|---|---|
| `chg-arkdeck-arkforge-authority/` | ArkDeck 保留 authority，把 Rockchip lowering 交给 ArkForge | **已复制进 ArkDeck**（2026-08-15），落在 `openspec/changes/chg-2026-059-arkdeck-arkforge-authority/`；四份稿件 2026-08-19 起带 **NRU-004 超越声明**——fixed-tool 执行面已退役，照抄启动契约会失败 |
| `chg-agent-native-cli/` | ArkForge 统一 Agent-native CLI、独立 CLI authority 与原生 RockUSB 救援面 | **software-complete / hardware-gated**（2026-08-20）；typed tree、direct authority、normal plan/apply、managed HDC、no-replay、JSONL 与原生救援已落地，三个旧 CLI 均已移除；CLI-AC-28..32 等待受控真机证据与 exact support-key review |

> 本目录留的是**草稿正本**；ArkDeck 里那份是按它的约定改过的落地版。
> 两处若要再改，改 ArkDeck 那份——它已经进了那个仓的治理流程，
> 这里这份的作用只剩「依据在哪」。

`chg-arkdeck-arkforge-authority/` 里的五份文件：

| 文件 | 给谁看 |
|---|---|
| `proposal.md` | 维护者 review：要不要做、为什么、影响面 |
| `design.md` | **实现者：怎么做**。含 CBOR 编码规则、线上往返、超时实测值、失败处置 |
| `tasks.md` | 单任务垂直交付的范围与允许改动路径 |
| `verification.md` | 九条验收与各自的证据形式 |
| `permit-vectors.md` | 三组交叉验证向量，支撑 `AFA-AC-2` |

## 落地时改了什么（chg-2026-059，已完成）

草稿是按通用形状写的，ArkDeck 有自己的约定，落地版做了这些调整：

| 项 | 草稿 | ArkDeck 落地版 |
|---|---|---|
| ID | `CHG-YYYY-NNN` | `CHG-2026-059`（059 是当时的下一个空号） |
| `status` | `draft` | `proposed` —— `draft` 不在 ArkDeck 的状态机里，它是 `proposed → approved → implementing → verified → archived` |
| `class` | `capability` | `integration`，并在 proposal 里写明「若维护者认为 destructive 执行跨进程构成 capability/core 变化，请重分类」 |
| `owner` | `lvye` | `fuhanfeng` |
| Requirements / Acceptance | 只在 tasks/verification 里引用 ID | 在 proposal 里补齐 `AFA-REQ-001…005` 与 `AFA-AC-1…9` 的正文——原稿引用了从未定义的 ID |
| `spec-impact.md` | 无 | 补：本 change 无 spec delta，但有两处需要维护者判断（class 归属、`REQ-FLASH-015` 的执行者身份） |
| `evidence/` | 无 | 补：目录结构 + 跨仓引用表，并说明为空的原因 |

## 若还要再贴别的

- `core_baseline` 按当时的 protected-main 值填（当前 `CORE-3.0.0`）；
- `verification.md` 的 `Environment` 按实际硬件与 toolchain digest 复核。
  NRU-004 之后不再有外部工具可绑：`arkforged` 原生执行，握手里的
  `toolchain_sha256` 是 daemon 自身构建摘要（AD-015 的 quarantine 教训
  只对历史 vendor 工具成立，留档即可，勿再写进新 Environment）。

## 与本仓的接口

提案引用的 ArkForge 侧产物，都是已经在仓里、可被 ArkDeck 侧测试直接对照的：

- `adapters/arkforge-arkdeck-adapter/src/lib.rs` — step 映射表
- `adapters/arkforge-arkdeck-adapter/src/control.rs` — 控制动作映射表 + ArkDeck
  provider action 的归属表（keptByAuthority / keptInternal / delegatedToArkForge）
- `chg-arkdeck-arkforge-authority/permit-vectors.md` — permit 编码的交叉验证向量，
  由 `crates/arkforge-authority-api/tests/permit_vectors.rs` 守卫
