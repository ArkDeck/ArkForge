# AFD-0001：ArkForge workspace 零第三方运行时依赖

> 状态：Accepted(实施决定，非架构变更)
>
> 日期：2026-08-14
>
> 相关：architecture.md 4.2 / 4.3 / 6.2 / 15.4 / 20.1、AFD-0002

## 背景

架构正本要求安全摘要用 RFC 8949 deterministic CBOR + SHA-256(6.2 / 15.4)，
要求 vendor tool 固定哈希、无 PATH 解析、不加载任意第三方动态插件(1.2 / 16.1 /
20.1)。实施环境另有一个硬约束：本机 crates.io 拉取不可用(index 可达，
crate 下载与 `cargo add` 均挂起，registry cache 为空)，因此依赖解析本身不可复现。

## 决定

ArkForge workspace 不引入任何第三方运行时依赖。以下原语在仓内实现，各自带公开
测试向量：

| 原语 | 位置 | 向量来源 |
|---|---|---|
| SHA-256 | `arkforge-core::digest::sha256` | FIPS 180-4 / NIST CAVP |
| deterministic CBOR | `arkforge-core::digest::cbor` | RFC 8949 Appendix A |
| DEFLATE / gzip | `arkforge-artifact::inflate` | RFC 1951 / RFC 1952；与系统 `gzip` 交叉验证 |
| CRC-32 | `arkforge-artifact::inflate::crc32` | RFC 1952 §8 |
| tar(ustar/GNU) | `arkforge-artifact::tar` | POSIX.1-1988 ustar；ArkDeck ARC00x 危险向量 |
| Protobuf wire codec | `arkforge-ipc::wire` | protobuf.dev encoding 规范 |

## 理由

1. **摘要即授权边界**。plan digest 决定 ArkDeck 是否放行一次破坏性写入。产生
   这些字节的代码必须在本仓 review 范围内，而不是一条 `sha2 = "0.10"`。
2. **与既有安全姿态一致**。架构已要求 vendor tool 固定哈希、禁止动态插件；
   对 Rust 依赖树采用同一标准是自洽的，不是额外苛刻。
3. **可复现**。无 registry 依赖 → 无 lockfile 漂移、无 supply-chain 更新窗口、
   离线可构建，符合 24.1「evidence 必须可复现」。
4. **环境事实**。当前主机无法从 crates.io 取包，依赖方案不可实施。

## 代价与缓解

- **async**：无 tokio/async-trait。AF-V1 全部为只读同步路径，authority SPI
  以同步 trait 表达(见 AFD-0002)。AF-V2 引入 durable engine 与真实 USB I/O 时，
  若需要 async，须单独决策并重估此项。
- **实现风险**：自写 DEFLATE/CBOR/protobuf 有出错空间。缓解是每个原语都对公开
  测试向量、与系统工具交叉验证(gzip)、并进入 `fuzz/` 目标。
- **维护成本**：这些原语是稳定的冻结规范(RFC 1951 于 1996 年定稿)，不是活跃演进面。

## 复核条件

出现以下任一情况时重开此决定：

- 生产环境需要 ArkForge 承担高吞吐压缩/解压，仓内实现成为瓶颈且有实测数据；
- AF-V2 的 durable engine 与 USB transport 证明同步模型不可行；
- 组织建立了可审计、可 pin、可离线复现的 Rust 依赖供应链。
