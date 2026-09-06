# 清理报告复核（2026-09-06）

复核基线：`ceb3aa7`。输入为 `ArkForgecleanupreviewhandoff.md` r2。
本页更正报告结论并记录修复；原始报告与捕获文件保持原字节。

| 报告项 | 独立复核与处置 |
| --- | --- |
| P1 历史 capture digest | 三项均复现旧 `arkforge/v1/device-facts\0` 域。拓扑体来自 location ID；descriptor 使用 VID/PID、bcdDevice=0x0223、厂商及产品名；serial 来自既有 2026-08-14 capture 记录。现行域下三项均不同。登记 SI-018，保留历史证据，增加整个文件字节 pin。不能将其用作现行身份摘要向量。 |
| P2 失效 CLI 测试 | 确认 `flash assess`、`flash apply` 不存在。改为 `artifact show`、顶层 `apply`，增加合法摘要、合法 observation、带 acknowledgement 的成功解析对照。两处 help 索引负断言增加进程成功检查。 |
| P3 Swift CI | 确认原 macOS 工作流未执行 Swift。在现有 job 增加根目录 `swift test`。本机执行不能代替远端新工作流运行。 |
| P4 mapping | Rust adapter 无 `terminal_outcome`，也没有可直接替换的同名职责实现。ArkDeck 的实际符号为 `ArkForgeFlashSession::terminalOutcome`。修正 informative mapping，去掉虚构的 Rust 测试覆盖，明确外部测试不在本 workspace 运行；登记 SI-019。 |
| P5 wire 测试 | 请求测试确实手写字段，绕过生产路径。将 controller 现有编码块提取为生产使用的私有编码方法，并把该向量测试从 IPC crate 移到 client crate；公共 assessment 请求也增加生产编码器字节检查。Swift 请求测试也已经调用 SDK 的 `ArkForgeMaterializePlanRequest.encoded`；响应测试原本已调用 `MaterializePlanResponse::encode`，内部调用 `Assessment::encode`，因此“两头都不设防”不准确。保留 Swift 独立向量，未更改 wire 或重新生成规范 fixture。完整 MaterializePlan/Assessment conformance fixture 扩展仍是可选后续工作，单独增加 fixture 不能替代生产路径验证。 |
| 5.3 clone | 去除 `partial.sealed_campaign` 最后使用处 clone；错误格式化借用 assessment 字段，保留后续借用。未改仍需使用的 `tokens.clone()`。 |
| 第 4 节抽查 | `CORE_SCHEMA_VERSION` / `is_enabled` 仓内无消费者这一观察成立，但属于公开 API；本次没有充分依据删除。闭包风格、跨平台对称签名、测试 helper 合并不构成本次必要修复。 |

历史文件 SHA-256：`301acc7d07623d71edc35881edb98923fae3e607f8cdac4dafe8c6b75b7470a9`。
按当前域重算的 topology / descriptor / serial digest 分别为：

- `3ec01c30971df27e26543c63b3856452cbae569f060278e2d10d021a68cfc1be`
- `82424fa8433c27775568db0b6422f05d97fcce7a47310ff40be9da609f73482a`
- `9fae766ff6fb79cfc24097bd906af7b5b0cd8eb6354a215d2a6ba7e4a29b2ed4`

这些值只是独立推导结果，不是新捕获证据。未执行设备操作，未修改 ArkDeck。
报告中的旧提交失配史、lint 数量及作者的历史实验未作为修复依据，也未逐一重做。

反向验证：临时移除 CLI 的 SHA-256 校验后，占位符测试在文件路径输入处失败；
临时漏写 controller 请求字段 13 后，生产编码器向量测试在字节比较处失败。
两次均为预期断言失败（退出码 101），随后恢复源码再执行完整检查。

验证结果：

- `cargo fmt --all -- --check`、完整 Clippy（所有 target/feature，`-D warnings`）通过。
- `cargo test --workspace --all-targets` 在解除沙箱限制后通过：51 个测试二进制，654 passed、1 ignored（原有大文件预算测试）。初次沙箱内运行与短临时路径单测均报 `DAEMON_UNAVAILABLE`，相同自动启动单测在沙箱外通过；未改测试断言规避失败。
- `cargo test -p arkforge-conformance` 通过，未重新生成 fixture。
- Swift 26 tests 通过。默认缓存目录受沙箱限制，实际成功命令为 `CLANG_MODULE_CACHE_PATH=/tmp/arkforge-review-clang-cache SWIFTPM_MODULECACHE_OVERRIDE=/tmp/arkforge-review-swift-cache swift test --disable-sandbox`。
- `git diff --check` 通过。Windows、远端 CI、ArkDeck 外部测试与真实设备操作均未运行。
