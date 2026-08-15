# 待贴进 ArkDeck 的 OpenSpec change

本目录里的东西**不属于本仓的治理流程**。它们是写好、可以整个目录复制进
`ArkDeck/openspec/changes/` 的提案草稿，因为它们描述的变更发生在 ArkDeck 仓里，
按 ArkDeck `AGENTS.md` 的控制平面条款必须走 OpenSpec + 维护者 PR review/merge。

放在这里而不是直接开 ArkDeck 的 PR，是因为提案的**依据**在本仓：
真机实测、读域证据、permit 编码。提案与依据不该分居两地。

| 目录 | 内容 | 前置 |
|---|---|---|
| `chg-arkdeck-arkforge-authority/` | ArkDeck 保留 authority，把 Rockchip lowering 交给 ArkForge | ArkForge 侧已完成到写入前最后一步，见 [2026-08-15 彩排](../evidence/runs/2026-08-15-dayu200-flash-rehearsal.md) |

`chg-arkdeck-arkforge-authority/` 里的四份文件：

| 文件 | 给谁看 |
|---|---|
| `proposal.md` | 维护者 review：要不要做、为什么、影响面 |
| `design.md` | **实现者：怎么做**。含 CBOR 编码规则、线上往返、超时实测值、失败处置 |
| `tasks.md` | 单任务垂直交付的范围与允许改动路径 |
| `verification.md` | 九条验收与各自的证据形式 |
| `permit-vectors.md` | 三组交叉验证向量，支撑 `AFA-AC-2` |

## 贴进去之前要改的

- frontmatter 的 `id` 与目录名里的 `CHG-YYYY-NNN` 换成实际编号；
- `status` 保持 `draft`，由维护者在 review 后改；
- `core_baseline` 按当时的 protected-main 值填；
- `verification.md` 里的 `Environment` 按实际硬件与工具 digest 复核——
  尤其是 rkdeveloptool：本仓自建的那份与 homebrew 的那份字节相同，
  但后者带 quarantine 时会挂死在 dyld（AD-015）。

## 与本仓的接口

提案引用的 ArkForge 侧产物，都是已经在仓里、可被 ArkDeck 侧测试直接对照的：

- `adapters/arkforge-arkdeck-adapter/src/lib.rs` — step 映射表
- `adapters/arkforge-arkdeck-adapter/src/control.rs` — 控制动作映射表 + ArkDeck
  provider action 的归属表（keptByAuthority / keptInternal / delegatedToArkForge）
- `chg-arkdeck-arkforge-authority/permit-vectors.md` — permit 编码的交叉验证向量，
  由 `crates/arkforge-authority-api/tests/permit_vectors.rs` 守卫
