# AF-V1 验收证据

> 日期：2026-08-14
>
> 范围：architecture.md 22 `AF-V1：ArkForge Core + DAYU200 read-only parity`
>
> ArkDeck 审计基线：架构正本记 `2849c5c1`；本次实施时本机 ArkDeck 为
> `60bfa76d fix(TASK-AIN-021): close typed runtime and LaunchAgent gaps (#1304)`。
> 差异为 TASK-AIN-021 收口，不触及 flash 执行路径与本文引用的 pinned 事实
> (`RockchipFlashProfile`、`partition-mapping.json`、`WorkflowStep.swift` 元数据)。
>
> 复现：`cargo test --workspace --offline`(338 tests，全绿)；
> 导入预算另见下文单独命令。

## 1. 生产代码交付

| architecture.md 22 要求 | 交付 |
|---|---|
| Rust workspace(八个边界 crate) | `crates/arkforge-{core,authority-api,artifact,transport,provider,engine,ipc}` + `crates/arkforged` + `adapters/arkforge-arkdeck-adapter` |
| neutral Authority API | `arkforge-authority-api`：`ExecutionAuthority`、`StepPermit`、`StepAdmissionSnapshot`、freshness 判定、`ManagedDeviceControlPort` |
| Artifact / CAS | `arkforge-artifact::cas`：quota、available-space preflight、lease 引用计数、crash-safe GC、0600 ACL |
| DAYU200 parser / profile | `arkforge-artifact::dayu200`(流式 gzip/tar + mtdparts 语法 + build facts)、`profiles/dayu200.yaml` |
| Rockchip read-only probe | `arkforge-provider::rockchip::probe`(只读；开同一 handle 重读 identity) |
| PlanAssessment / FlashPlan | `arkforge-core::plan`；`materialize_with_private_plan` 两分支 |
| deterministic digest | `arkforge-core::digest`：SHA-256(FIPS 180-4 向量)、RFC 8949 §4.2 CBOR(Appendix A 向量)、域分隔表 |
| projection validator | `arkforge-core::projection::validate_projection` |
| daemon read-only API | `arkforged`：UDS 双 socket、Protobuf 帧、15.3 全 API |
| golden transcript 库 | `transcripts/dayu200-gj4-ecamp-{96effff15,31e041bc}.yaml` |

## 2. 验收逐条

### 2.1 Core 不依赖 ArkDeck / vendor

守卫：`crates/arkforged/tests/architecture_guard.rs`。按 4.3 要求，**依赖图是主守卫**，
词法扫描是次守卫：

- `core_depends_on_nothing`：读 manifest，`arkforge-core` 的 `[dependencies]` 为空；
- `the_dependency_direction_matches_the_architecture`：逐条比对 4.3 允许的边；
- `no_crate_depends_on_the_daemon_or_the_adapter`：进程与某一 authority 的 adapter 是图的另一端叶子；
- `the_neutral_crates_name_no_device_vendor_or_authority_in_code`：core / authority-api /
  ipc / engine 的**代码**(剥离注释后)不出现 dayu200 / rockchip / unisoc / rkdeveloptool /
  arkdeck / uis7885 等名字。为通过此项，这些 crate 的测试夹具已改用中性名
  (`org.example.testboard`、`example-tool-fixed`、`test-authority`)——证明中性 crate
  连测试都不需要设备名，而不是给它们开豁免；
- `the_daemon_never_mints_a_permit`：`arkforged` 不出现 `authority_side` / `mint_integrity_tag`；
- `the_public_plan_surface_carries_no_vendor_vocabulary`：类型边界检查，
  `FlashStepKind` 词表与其规范编码不含 vendor / 地址词汇。

### 2.2 current DAYU200 archive facts parity

- `crates/arkforge-core/tests/dayu200_profile_parity.rs`：9 个可写目标的
  partition / writeOrder / offsetSectors / sourceMember 与 ArkDeck
  `RockchipFlashProfile.dayu200.mappedPartitions` 逐值一致；
  `protectedTargets` 与 `membershiplessPartitionsWriteForbidden` 逐值一致；
  `chip_prod.img` / `sys_prod.img` / `MiniLoaderAll.bin` 不支撑任何可写目标。
- `crates/arkforge-artifact/src/dayu200.rs::decodes_the_pinned_dayu200_partition_table`：
  15 个分区的 name / offset / size / grammarBranch 与
  `partition-mapping.json`(schema `arkdeck-dayu200-partition-mapping-1.0.0`)逐值一致。
- `crates/arkforge-artifact/tests/dayu200_inspect.rs`：import → inspect 复现 17 成员清单
  与角色划分。

**真机补充(2026-08-14)**：三方一致中的**设备一侧已由真机验证**。板子自身的 GPT
(`rkdeveloptool ppt`)与本仓解码 **15/15 逐值一致**，profile 的 9 个可写目标 + 6 个
protected 目标恰好覆盖设备表全部分区。见
[只读取证](runs/2026-08-14-dayu200-read-only-capture.md) §4 与
`crates/arkforge-core/tests/dayu200_real_device_parity.rs`。

**仍存边界**：与厂商真实 `images.tar.gz`(`fc7637f3…` / `6a023c73…`)的**字节级**比对
仍不在范围——该归档不在任一仓库内。夹具是结构等价(成员清单、角色、分区表相同，
成员体为确定性小数据)，`fixture.rs` 顶部对此明写。

### 2.3 unknown member / partition fail closed

- parser 对无法归类的成员产出 `RK-A02` unknown 而不猜测；
- Profile 的 `artifactCompatibility.knownMetadataMembers` 是唯一解除途径
  (`updater_binary` 由此解除，与 ArkDeck 把它归为 `nonPartitionMetadata` 一致)；
- 未被 Profile 声明的成员仍然阻断：
  `dayu200_vertical.rs::an_artifact_with_an_unaccounted_member_blocks_execution`；
- profile offset 与 artifact 分区表不一致 → `RK-V05` 阻断
  (`a_profile_offset_that_disagrees_with_the_artifact_table_blocks_execution`)；
- 缺失镜像成员 → `RK-V07` 阻断。

### 2.4 private action digest 覆盖

`dayu200_vertical.rs::the_executable_branch_produces_a_fully_projected_sealed_plan`：
私有计划每个 action 的 digest 都出现在 `plan.per_action_digests` 中；对已封计划重跑
projection 复现 `providerExecutionPlanDigest` 与 `publicProjectionDigest`。
篡改私有 body 立即打断与 public step 的绑定
(`projection.rs::a_tampered_private_body_breaks_the_step_binding`)。

### 2.5 startExecution disabled

三层，各自独立：

1. 类型层：`arkforge_engine::ExecutionGate` 没有「允许」变体；
2. 服务层：`arkforged` 对 `API_START_EXECUTION` 返回 `STATUS_UNAVAILABLE`
   (`api_surface.rs::start_execution_is_unavailable_on_the_controller_socket`)；
   public socket 上先被 session 规则拒绝(`SESSION_NOT_PERMITTED`)；
3. 线上层：真实启动 daemon、走 UDS 与握手后仍为 `UNAVAILABLE`
   (`socket_roundtrip.rs::the_daemon_serves_the_read_only_vertical_over_unix_sockets`)。

此外 maturity 组合键使 AF-V1 的 fixed-tool 组合为 `HardwareGated`、replay 组合为
`PlanOnly`，因此 materialize 只出 assessment。

### 2.6 unit / fuzz / transcript tests

- unit：338 tests，`cargo test --workspace --offline` 全绿；
- 原语对公开向量：SHA-256(FIPS 180-4 四向量 + 百万 a)、HMAC-SHA-256(RFC 4231)、
  CRC-32(RFC 1952 §8)、CBOR(RFC 8949 Appendix A)、DEFLATE(与系统 `gzip -1/-6/-9`
  在 6 组语料上交叉验证)、tar(与系统 `tar` 互操作)；
- fuzz：`crates/arkforge-artifact/tests/parser_fuzz.rs`，18000 个 seeded 变异输入，
  性质为「不 panic、不挂起、不无界分配」(含 PAC 观测器 5100 例)，见 [`fuzz/README.md`](../../fuzz/README.md)；
- transcript：`crates/arkforge-transport/tests/golden_transcript_parity.rs`，
  两个 GJ-4 campaign 的 13 步收据链、每个 digest 由声明的推导规则复算。

### 2.7 Profile 含 readDomain 与 per-target 验证强度，与 AD-006 一致

`profiles/dayu200.yaml`：

```yaml
readDomain:
  write: full-disk               # wlx 全盘可达
  read: characterize-at-runtime  # rl 读窗每次实测
  erasedMediumFiller: 0xCC
```

- `the_read_domain_encodes_ad006` 另断言 profile 源文件**不得**写入 65536——
  那是一次会话的观测，不是所有板子的常量；
- 9 个目标各自声明 `maxStrengthWhenReadable` 与**必需的** fallback；
  `ProfileError::VerificationWithoutFallback` 使「读域运行时实测却无兜底」不可加载；
- 三态判定在 `arkforge-core::verification`：读域不覆盖 → `TypedSkip`(不计任何 verified 强度)；
  读域覆盖且 uniform filler → `Failed{ErasedMediumFiller}`(单列，不冒充 hash mismatch)；
  读域**不**覆盖且 uniform filler → **不是失败**——这正是 2026-08-04 九个分区被冤判的那一类
  (`uniform_filler_outside_the_window_is_never_a_failure`)；
- **真机复现(2026-08-14，AD-009)**：读窗边界实测落在扇区 65536，窗口外(含 system 245760、
  vendor 4440064)恒返回 uniform `0xCC`——而板子当时正由这两个分区启动运行
  OpenHarmony-7.0.0.37。窗口外的 `0xCC` 因此被现场证明**不等于「未写入」**。

### 2.8 DAYU200 整包 CAS 导入在声明预算内

声明预算：60 s(10.2 的 ~10 GB/min 锚点下，730 MiB 约 4.4 s；60 s 是宽松上限，
但足以在导入路径长出每字节成本时立刻变红)。

实测(release，本机 macOS/aarch64)：

```text
import: 730769584 bytes in 3.07s (227.0 MiB/s), budget 60s
verify: 2.34s
```

复现：

```bash
cargo test -p arkforge-artifact --release --test cas_import_budget -- --ignored --nocapture
```

available-space preflight 实测：`the_available_space_preflight_refuses_a_bundle_that_would_not_fit`
在真实 bundle 尺寸上验证「差一字节则拒绝、够一字节则接受」，且拒绝发生在**任何字节被复制之前**。

**边界**：被计时的是本实现在真实尺寸上的导入路径(流式读 + 全量 SHA-256 + staging +
fsync + rename)，输入为等长确定性合成流，不是厂商归档本身。内容寻址必须哈希每一字节，
两者工作量同形。

### 2.8b 工具身份与模式判定(2026-08-14 真机)

- **AD-010 已解**：ArkDeck 的两个 pin 是设计(只读发现 / 破坏性刷写各一个本地构建，
  同一 upstream commit)，`038a8a0e…` 逐字节命中 `~/dayu200-rehearsal/rkdeveloptool/`。
  不存在「签名前/后」歧义。`ToolchainIdentity` 因此新增 `upstream_ref`——digest 仍是
  判别量，provenance 只是让 receipt 说得清是哪个构建。
- **AD-013 开着，且是本次最要紧的一条**：`rkdeveloptool ld` 把处于 HDC-normal 的
  DAYU200 报成 `Mode=Maskrom`(三次复现)。ArkForge 结构性免疫：模式来自 Profile 声明的
  实测 VID/PID，transport 从不读厂商工具的 mode 词。
  `usb.rs::a_pid_the_vendor_tool_misreports_still_resolves_by_profile` 钉住这条。

### 2.9 无设备 mutation

- AF-V1 唯一的 transport 是 transcript replay，它对 transcript 未记录的动作返回
  `Unsupported` 而不是模拟(`an_action_the_transcript_never_recorded_is_unsupported_not_invented`)；
- `probing_never_touches_a_write_path` 断言 `write-partition` / `erase-partition` / `wlx`
  在只读 campaign 位置上不可回放；
- Provider SPI 的执行侧方法默认拒绝(`execution_side_spi_methods_refuse_in_this_build`)；
- 本仓不存在 USB 后端与 vendor 可执行调用路径。

## 3. 额外交付(AF-V1 未列出但架构要求)

- `adapters/arkforge-arkdeck-adapter`：published `FlashStepKind ↔ WorkflowStep kind`
  映射表(5.4)，含 ArkDeck registry 的 effect / cancellation / binding 下限。
  `the_materialized_dayu200_plan_would_be_admissible_by_the_arkdeck_registry` 验证本仓
  materialize 出的 23 步计划逐步映射到已发布 kind 并满足全部下限。
  `LoadEphemeralAgent` 显式 `Unmapped` 并给出理由——ArkDeck registry 没有对应条目，
  含该 kind 的计划在条目发布并 review 前不可准入(fail closed，不是绕过)。
  该表的一个直接后果：ArkDeck 的 effect 阶梯没有 transient 档，`enterUpdater` /
  `rebootDevice` 的下限落在 `Mutating`，因此 Rockchip provider 的模式变更步骤按
  `Mutating` 声明(over-declare 只会收紧准入)。
- `proto/arkforge.proto`：IPC 正本 schema，含演进规则(未知 field 跳过 / 未知 enum 硬失败)。
- `arkforge-cli`：只读诊断，连 public socket，结构上无法 import 或 execute。

## 4. 已知边界(不在 AF-V1 范围，明写以免被误读为已完成)

| 项 | 状态 | 归属 |
|---|---|---|
| 真实 DAYU200 刷写 | 未做，本环境无硬件 | AF-V2 |
| durable engine / journal fsync / crash campaign | 仅记录模型与链校验，无耐久性验收 | AF-V2 |
| StepPermit 消费与 receipt 落盘 | 仅类型与校验逻辑，无执行路径 | AF-V2 |
| ArkDeck 侧接入(authority adapter 实现、UI、compatibility alias) | 未做；属另一仓库且须 OpenSpec + 维护者 review | AF-V2 |
| arkforged 签名 / entitlement / 打包契约 | 未做 | AF-V2(见 21.2 Stage B、AD-007) |
| Windows Named Pipe | 设计预留，已明示退出 v1 验收 | 15.2 |
| 厂商归档字节级 parity | 未做，归档不在仓内 | 取得归档后的证据项 |
| DAYU600 任何执行能力 | 无；证据门未过 | AF-V3 / AF-V4 |
