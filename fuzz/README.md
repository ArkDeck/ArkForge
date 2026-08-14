# fuzz

## 现状

本目录不含 cargo-fuzz 目标。原因是环境事实：本机 crates.io 不可达
(见 [AFD-0001](../docs/decisions/AFD-0001-zero-dependency-core.md))，`libfuzzer-sys`
与 `cargo-fuzz` 都取不到；同时 AFD-0001 决定 workspace 不引入第三方运行时依赖。

因此 fuzz 以**确定性变异测试**形式实现在常规测试目标内，随 `cargo test` 每次运行：

| 目标 | 位置 | 覆盖 |
|---|---|---|
| DAYU200 archive | [`crates/arkforge-artifact/tests/parser_fuzz.rs`](../crates/arkforge-artifact/tests/parser_fuzz.rs) | gzip 容器 + tar 框架 + parameter.txt 语法，4000 个变异输入 |
| gzip / DEFLATE | 同上 | 6000 个变异输入 |
| tar | 同上 | 6000 个变异输入 |
| parameter 语法 | 同上 | 6 个语料 × 2000 个变异 |

被测性质不是「解析成功」，而是**任何输入都不会 panic、不会挂起、不会无界分配**：
每次拒绝都必须是 typed error。arkforged 持有设备权限，parser panic 就是它的
拒绝服务面(architecture.md 20.1)。

变异是 seeded(xorshift64\*)，不是随机：失败必须能由断言里打印的 seed 复现，
CI 不会因为某天恰好抽到一个坏字节串而变红。

## 迁移到 cargo-fuzz

当具备可审计、可 pin、可离线复现的 Rust 依赖供应链后(AFD-0001 复核条件之一)：

1. `cargo fuzz init`；
2. 为每个 `tests/parser_fuzz.rs` 中的性质建立一一对应的 `fuzz_target!`，
   入口函数保持相同(`dayu200::inspect`、`GzipReader`、`TarReader`、
   `dayu200::parse_parameter`)；
3. 保留 `tests/parser_fuzz.rs`——它是回归门，cargo-fuzz 是探索器，两者不互相替代；
4. 把 cargo-fuzz 发现的 crash 输入固化进 `tests/` 语料。

## 语料

`crates/arkforge-artifact/src/fixture.rs` 生成的 DAYU200 归档形状是主语料种子：
17 个成员、真实分区表、内嵌 build facts。它的成员清单与 ArkDeck
`member-inventory.json` 一致，因此变异是围绕真实结构展开，而不是围绕随机字节。
