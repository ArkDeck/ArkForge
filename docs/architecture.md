# 面向 ArkDeck 的设备无关刷机平台 ArkForge

> 文档状态：Proposed
>
> 修订日期：2026-08-13(同日第二轮修订：以 GJ-4 真机定案事实与仓内生产实现复核)；2026-08-14 定名并迁入独立工程目录
>
> 命名：原案名 ArkFlash；2026-08-14 定名 ArkForge，ArkFlash 名称保留给未来面向用户的刷机 UI 产品位
>
> ArkDeck 审计基线：本地 origin/main 2849c5c188717ac351f9228a9cd60c054035fbcf
>
> 目标设备：DAYU200、DAYU600，以及后续采用其他 SoC、固件格式和下载协议的产品
>
> 安全声明：本文是架构提案，不构成代码批准、Provider 发布或设备执行授权。生产启用新 Operation、Provider、integration/device profile 或 destructive admission 变化，必须经过 ArkDeck 规定的 OpenSpec 与维护者 PR review。

---

## 0. 执行摘要

### 0.1 最终结论

ArkForge 应建设为独立的、设备无关的刷机工程，负责抹平以下差异：

- 固件容器和镜像布局；
- 芯片厂商下载协议；
- USB transport、设备模式和重枚举；
- boot agent、loader、FDL 等临时阶段；
- 分区、地址、擦除、写入、校验和 reboot 的具体实现；
- 厂商工具或原生协议的语义输出；
- provider-specific reconcile 与人工恢复建议。

ArkDeck 不应再包含：

- rkdeveloptool、CmdDloader 或其他厂商工具命令；
- RockUSB、Unisoc Download、PAC、FDL 等协议分支；
- VID/PID、接口、endpoint 和分区地址判断；
- 厂商 stdout marker；
- 设备型号到执行命令的分支。

但是 ArkForge 不能隐藏或自行决定以下安全事实：

- 精确目标身份和跨模式 lineage；
- 完整持久化与瞬态 EffectSet；
- userdata、校准、NV、安全分区等数据影响；
- ordered public step set；
- provider/profile/artifact/toolchain identity；
- cancellation boundary；
- outcomeUnknown、uncertainEffectSet 和恢复覆盖范围；
- 每个实际执行动作的不可变摘要和 semantic receipt。

最终推荐：

1. ArkForge 使用 Rust 实现；
2. 生产形态使用本地 daemon；
3. macOS/Linux 使用 Unix Domain Socket，Windows 使用 local-only Named Pipe；
4. IPC 使用版本化 Protobuf；
5. 安全摘要使用 RFC 8949 deterministic CBOR + SHA-256；
6. Provider 初期静态注册；
7. ArkDeck 通过独立的 arkforge-arkdeck-adapter 接入；
8. DAYU200 首版继续封装固定哈希 rkdeveloptool；
9. DAYU600 在证据门通过前只支持 inspect 和非可执行 PlanAssessment；discover/probe 待对应只读 USB identity 证据取得后开放(当前 UNI-U01 = missing，见 17.1)；
10. outcomeUnknown 永不 replay，但保留 ArkDeck 已有的 distinct complete-overwrite supersession recovery。

### 0.2 最重要的修订

本版相对原报告修正了以下架构问题：

- 不再把 flash.execute-plan 作为 Agent/App 可提交的公开 Operation；
- ArkForge Core 不再出现 arkdeck_binding_id 等 ArkDeck 专用类型；
- Provider 私有执行计划必须以 providerExecutionPlanDigest 和 per-action privateActionDigest 进入授权边界；
- reconcile 保持严格只读，但新增独立的 SupersedingRecoveryPlan；
- 原 outcomeUnknown 永不改写，恢复使用新的 plan、capability、reservation、intent 和 epoch；
- StepPermit 补全 fresh facts、nonce、expiry、single-use 和 crash idempotence；
- HDC server 仍由 ArkDeck 管理，ArkForge 只能使用 typed ManagedDeviceControlPort；
- Provider maturity 改为 provider/profile/artifact/toolchain/platform 组合级状态；
- UNKNOWN effect 只能形成 PlanAssessment，不能形成可执行 FlashPlan；
- ADR 状态从 Accepted 改为 Proposed；
- 实施任务由十五个水平任务收敛为四个垂直产品任务。

本版第二轮修订(以 GJ-4 真机定案事实与仓内生产实现复核)：

- DAYU200 verification 从无条件 ReadbackPartition 改为 read-domain-aware 三态模型(Verified / TypedSkip / Failed)，对齐 `rl` 读窗(2026-08-04 实测扇区 65536 / 32 MiB 起盲区)与 uniform 0xCC 定案；
- DeviceProfile 新增一等 readDomain 事实与 per-target 可达验证强度声明；
- RebindExpectation 新增瞬态观测容忍策略，模式别名等价关系上收为 Profile 事实；
- StepPermit 补写 integrity tag 信任模型：铸造方、密钥背书、重启生命周期、重传存储；
- freshness 改为以 session/handle 连续性为主事实，墙钟降为兜底，deadline 按 step kind 预算；
- WorkflowStep registry 兼容要求从 Core 移入 arkdeck-adapter 的 published 映射表；
- arkforged 的 macOS 签名/entitlement/打包契约对齐(#1299 体系与 entitlement 死锁教训)列为 Stage B 显式工作项；
- evidence ledger 新增 AD-006(读写面不对称定案)与 AD-007(entitlement 死锁)，仓内证据改为仓相对路径；
- DAYU600 证据门前 discover/probe 表述统一为条件式；
- Windows IPC 明确为设计预留，退出 v1 验收范围；
- CAS 大文件导入给出实测锚点与验收预算；
- AF-V1/AF-V2 任务补 golden transcript、read-domain 验收与打包工作项。

### 0.3 一句话边界

~~~text
ArkDeck 决定：谁、对哪台设备、以哪个已发布 Operation、在什么安全边界下执行。

ArkForge 决定：该已授权语义计划如何通过具体固件格式、Provider 和 Transport 正确落地。
~~~

---

## 1. 目标与非目标

### 1.1 产品目标

ArkDeck 对 DAYU200、DAYU600 和未来设备使用同一套接入代码：

~~~text
importArtifact
  → inspectArtifact
  → discoverDevices
  → probeDevice
  → materializePlan
  → Runtime validate/admit
  → startExecution
  → watch/cancel/reconcile
  → optional planSupersedingRecovery
  → result/recoveryGuide
~~~

新增设备时，变化应限制在 ArkForge 内：

- 同协议、同固件格式、同 effect model：优先只新增 DeviceProfile 与测试；
- 新固件格式：新增 Artifact Parser；
- 新协议：新增 Transport + Provider；
- 新执行工具：Provider 内新增 fixed tool backend；
- 新验证方式：新增 typed verification capability；
- ArkDeck production lowering 不增加厂商或型号分支。

### 1.2 非目标

ArkForge v1 不负责：

- 替代 ArkDeck RuntimeCapability；
- 决定 Agent 是否有权执行 destructive operation；
- 接受 raw executable、argv、shell、USB packet 或 vendor option；
- 提供通用 flash(file) 接口；
- 自动猜测设备型号或固件兼容性；
- 在 outcomeUnknown 后重发原写入；
- 在 DAYU600 协议证据不足时提供 experimental execute flag；
- 加载任意第三方动态插件；
- 宣称 hosted CI build 等于真实硬件支持。

### 1.3 ArkForge 的独立性

ArkForge 是独立工程，不把 ArkDeck 类型放入 Core。

ArkDeck 是 v1 的首个生产 Authority Adapter，而不是 Core 的硬编码依赖。

~~~text
arkforge-core
arkforge-authority-api
arkforge-artifact / arkforge-transport / arkforge-provider
arkforge-engine
arkforge-ipc
arkforged
        ↑
arkforge-arkdeck-adapter
        ↑
ArkDeck Runtime
~~~

ArkDeck 分发包只允许注册 ArkDeck authority adapter。未来若 ArkForge 需要独立产品形态，可以实现另一套 Authority，但它不能被 ArkDeck 部署自动发现或降级使用。

命名上，本工程定名 ArkForge(机械层：把已授权语义计划锻成设备字节)；ArkFlash 名称保留给未来可能的面向用户刷机 UI 产品，避免机械层与 UI 品牌互占。

---

## 2. 审计基线与证据等级

### 2.1 ArkDeck 基线

本文以本地可见的 origin/main 为代码审计基线：

~~~text
2849c5c188717ac351f9228a9cd60c054035fbcf
refactor(TASK-AIN-021): adopt app concurrency defaults (#1302)
~~~

原报告使用的 60f14eee 仍可作为历史固定点。其后的 #1299 修改了 macOS helper signing、entitlement、LaunchAgent 和 packaging，直接影响 arkforged 的签名、启动、身份与工具信任设计；#1300–#1302 为 TASK-AIN-021 现代化重构(macOS 26 API、生成式 localization、并发默认值)，经复核不触及 flash 执行路径。实施前必须以当时最新 main 再做一次差异复核。

另一条必须进入 arkforged 打包设计的仓内定案：macOS Rockchip 组件曾出现 entitlement 死锁——运行时校验器要求 app-sandbox+inherit，而打包契约(#1052)要求空 entitlements，两者互斥无解，最终以修改校验器收口，spec/ADR 对齐仍留白。新的 Rust daemon + 捆绑固定哈希工具会原样踏入同一区域，打包契约必须对齐当前校验器语义设计(见 21.2 Stage B 与 AD-007)。

**该留白已于 2026-08-16 对齐**：见 [AFD-0003](decisions/AFD-0003-arkforged-signing-packaging.md)。结论是两个二进制的 entitlement 字典都为空，并由 `arkforged` 在绑定工具之前读 Mach-O 代码签名强制；`arkforged` 不注册第二个 LaunchAgent，而是由 `arkdeck-agentd` spawn、pairing secret 走继承的 stdin。同一份实测查出 AD-023：ArkDeck 当前钉给破坏性 flash 的那份工具链接 Homebrew libusb，不可出厂。

### 2.2 已确认的 DAYU600 静态证据

仓库中的 bluetool-analysis.md 在 60f14eee 基线已经存在，并记录：

- BlueTool 包含 CmdDloader.exe、UNISOC DLL 和 PAC 资源；
- ohos.boot.hardware 返回 uis7885，用于 DAYU600/PAC 路径；
- DAYU600/PAC 与 DAYU200/RockUSB 是不同刷机实现。

这些证据支持“DAYU600 应采用独立 Unisoc Provider”的架构结论，但不能证明：

- PAC 完整格式；
- FDL1/FDL2 地址、顺序和安全握手；
- Download USB packet；
- exact USB identity；
- erase/write/verify/recovery 语义。

因此 DAYU600 execute 仍为 UNKNOWN/UNAVAILABLE。

### 2.3 证据等级

| 等级 | 定义 | 可支持的结论 |
|---|---|---|
| A | 官方规范、官方文档、官方源码的固定 revision | 可用于架构与公开协议事实 |
| B | 官方实现行为，但不是稳定规范 | 可用于 fixed adapter，必须 pin 版本并做 contract test |
| C | ArkDeck accepted spec、Catalog 和固定 SHA 源码 | 可用于迁移兼容与安全要求 |
| D | 社区逆向、实验记录、第三方工具 | 只用于提出研究假设 |
| U | 未取得、冲突或不可复现 | 必须 UNKNOWN/UNAVAILABLE |

外部证据必须固定 commit/tag。指向 master/main 的链接只用于导航，不构成可复现 evidence。

---

## 3. 权限与责任边界

### 3.1 ArkDeck Runtime 独占的责任

ArkDeck Runtime 必须继续独占：

- 已发布 Operation 的 admission；
- RuntimeCapability 的生成、reserve、consume 和期限；
- exact target binding 与跨模式 lineage 的最终判断；
- 全局 device-exclusive lane；
- exact typed inputs、Artifact lease 和 plan digest 绑定；
- 每个 external effect 前的 Runtime intent；
- fresh target/binding/tool facts 验证；
- closed invocation 的 16 epoch、四小时、并发一预算；
- outstanding intent 的 uncertainEffectSet union；
- safeToReflash 和 safeToSupersedeByCompleteOverwrite 的派生；
- SupersedingRecoveryEpoch；
- 最终 Job disposition 和 Agent 可见能力边界。

ArkForge 不解析 RuntimeCapability，也不能：

- mint；
- install；
- revoke；
- widen；
- renew；
- reinterpret；
- 把 evidence 或 manifest 当 authority。

### 3.2 ArkForge 独占的责任

ArkForge 负责：

- Artifact 导入、解析和 manifest；
- DeviceProfile 与固件兼容规则；
- Provider/Transport capability negotiation；
- immutable public plan；
- immutable private execution plan；
- public/private plan projection；
- typed ProviderAction；
- USB/tool dispatch；
- semantic receipt；
- operational journal；
- provider-specific read-only reconcile；
- complete-overwrite coverage declaration；
- recovery plan materialization；
- human recovery guide。

### 3.3 共享边界

两者通过以下不可变对象协作：

~~~text
PlanAssessment
FlashPlanEnvelope
PublicStepSet
EffectSet
ProviderExecutionPlanDigest
StepAdmissionSnapshot
StepPermit
ActionReceipt
PossibleEffectSet
RecoveryCoverageProof
FlashResult
~~~

任何一方都不能仅凭自己的 journal 宣称另一方已经确认成功。

---

## 4. 总体架构

### 4.1 Context

~~~mermaid
flowchart LR
    A[Human / AI Agent] --> APP[ArkDeck App / CLI]
    APP --> RT[ArkDeck Runtime]

    RT --> OP[Published semantic Operation]
    RT --> ADC[arkforge-arkdeck-adapter]
    ADC --> IPC[Versioned local IPC]
    IPC --> D[arkforged]

    D --> E[ArkForge Engine]
    E --> PR[Static Provider Registry]
    PR --> RK[Rockchip Provider]
    PR --> UNI[Unisoc Provider]

    RK --> ART200[DAYU200 Artifact Parser]
    RK --> RKT[RockUSB fixed-tool/native Transport]

    UNI --> PAC[PAC Parser]
    UNI --> UT[Unisoc Download Transport]

    E --> AJ[(ArkForge operational journal)]
    RT --> RJ[(ArkDeck authority journal)]

    E -->|StepAdmissionRequest| ADC
    ADC -->|Runtime intent then StepPermit| E
    E -->|ActionReceipt| ADC

    RK --> MDC[ManagedDeviceControlPort]
    MDC --> HDC[ArkDeck-managed HDC]

    RKT --> DEV[Physical device]
    UT --> DEV
~~~

### 4.2 建议的首版 workspace

首版不建议一开始拆成二十多个 crate。建议先保持八个稳定边界：

~~~text
arkforge/
├── crates/
│   ├── arkforge-core
│   ├── arkforge-authority-api
│   ├── arkforge-artifact
│   ├── arkforge-transport
│   ├── arkforge-provider
│   ├── arkforge-engine
│   ├── arkforge-ipc
│   └── arkforged
├── adapters/
│   └── arkforge-arkdeck-adapter
├── profiles/
├── proto/
├── fixtures/
├── transcripts/
└── fuzz/
~~~

DAYU200、PAC、Rockchip、Unisoc 先作为对应边界 crate 内的独立 module。第二个成熟 Provider 接入后，再根据编译隔离、许可证或发布需求拆成单独 crate。

### 4.3 依赖方向

~~~text
core
  ↑
authority-api / artifact / transport / provider
  ↑
engine
  ↑
ipc / daemon

arkdeck-adapter → authority-api + ipc client
~~~

Core 和 API 层不得依赖：

- dayu200/dayu600；
- Rockchip/Unisoc；
- PAC/FDL；
- rkdeveloptool；
- ArkDeck；
- Swift/AppKit/WinUI。

使用依赖图和类型边界测试，不使用简单的子字符串扫描作为唯一守卫。

---

## 5. 核心领域模型

### 5.1 中性 Authority 引用

~~~rust
pub struct AuthorityBindingRef {
    pub authority_namespace: AuthorityNamespace,
    pub binding_id: OpaqueId,
    pub binding_revision: u64,
    pub stable_identity_digest: Sha256,
}
~~~

ArkForge 不解释 authority_namespace 的业务语义。ArkDeck adapter 将 ArkDeck target binding 映射为该对象。

### 5.2 PlanAssessment 与 FlashPlan 分离

证据不完整时不能生成 executable FlashPlan。

~~~rust
pub enum PlanMaterialization {
    Executable(FlashPlanEnvelope),
    Assessment(PlanAssessment),
}

pub struct PlanAssessment {
    pub provider_candidates: Vec<ProviderCandidate>,
    pub profile_candidates: Vec<ProfileCandidate>,
    pub known_effects: EffectSet,
    pub unknowns: Vec<ExecutionUnknown>,
    pub evidence_requirements: Vec<EvidenceRequirement>,
    pub availability: ExecutionAvailability,
}
~~~

PlanAssessment：

- 可以展示；
- 可以导出研究证据；
- 没有 executable planID；
- 不能传入 startExecution；
- 不能触发 RuntimeCapability。

DAYU600 在证据门前只能返回 PlanAssessment。

### 5.3 FlashPlanEnvelope

~~~rust
pub struct FlashPlanEnvelope {
    pub schema_version: PlanSchemaVersion,
    pub plan_id: PlanId,
    pub plan_digest: Sha256,

    pub authority_binding: AuthorityBindingRef,
    pub provider: ProviderIdentity,
    pub profile: DeviceProfileIdentity,
    pub artifact: ArtifactIdentity,
    pub toolchain: ToolchainIdentity,

    pub negotiated_capabilities: NegotiatedCapabilities,
    pub public_steps: Vec<PublicFlashStep>,
    pub effect_set: EffectSet,

    pub provider_execution_plan_digest: Sha256,
    pub public_projection_digest: Sha256,
    pub per_action_digests: Vec<ActionDigestBinding>,

    pub recovery_contract: Option<RecoveryContractRef>,
    pub postflight: PostflightPolicy,
    pub created_at_epoch_ms: u64,
    pub expires_at_epoch_ms: u64,
}
~~~

### 5.4 Public step

Public step 词表 `FlashStepKind` 属于 ArkForge Core，保持设备与 authority 中立。与 ArkDeck WorkflowStep registry 的兼容不由 Core 承担：arkforge-arkdeck-adapter 维护一张 published 的 `FlashStepKind ↔ WorkflowStep kind` 映射表(进入 adapter 版本与 review 范围)，ArkDeck Runtime 按映射后的 registry 语义做 admission，Core 不引用 registry 类型：

~~~rust
pub struct PublicFlashStep {
    pub step_id: StepId,
    pub kind: FlashStepKind,
    pub effect: WorkflowEffect,
    pub cancellation: CancellationPolicy,
    pub binding: BindingRequirement,
    pub semantic_target: Option<SemanticTarget>,
    pub content_digest: Option<Sha256>,
    pub expected_mode_before: Option<DeviceMode>,
    pub expected_mode_after: Option<DeviceMode>,
    pub private_action_digest: Sha256,
}
~~~

ArkDeck Runtime 在 admission 前必须验证：

- kind 经 adapter 映射表对应到已发布 WorkflowStep kind；
- effect 不低于 registry minimum；
- cancellation 不弱于 registry；
- binding 满足 operation policy；
- profile 允许该 semantic target；
- step 数量和顺序在 Operation bounds 内；
- public EffectSet 覆盖每个 step；
- private_action_digest 已进入 providerExecutionPlanDigest；
- provider/profile/toolchain 组合已发布。

### 5.5 EffectSet

~~~rust
pub struct EffectSet {
    pub persistent: Vec<PersistentEffect>,
    pub transient: Vec<TransientEffect>,
    pub data_impact: DataImpact,
}

pub enum PersistentEffect {
    ErasePartition {
        partition: PartitionId,
        range: ByteRange,
    },
    WritePartition {
        partition: PartitionId,
        range: ByteRange,
        content: Sha256,
    },
    WriteRawRegion {
        region: RegionId,
        range: ByteRange,
        content: Sha256,
    },
    ReplacePartitionTable {
        layout_digest: Sha256,
    },
    ChangeBootMetadata {
        field: BootMetadataField,
        expected_value: TypedValue,
    },
}

pub enum TransientEffect {
    EnterMode {
        from: DeviceMode,
        to: DeviceMode,
    },
    LoadEphemeralAgent {
        stage: AgentStage,
        memory_region: MemoryRegion,
        content: Sha256,
    },
    UsbDetachReattach {
        expectation_digest: Sha256,
    },
    Reboot {
        target_mode: DeviceMode,
    },
}
~~~

以下任一状态使 executable plan 不成立：

- userdata impact unknown；
- calibration/security/NV impact unknown；
- wildcard write range；
- unknown execution-relevant manifest field；
- private action 无 public projection；
- semantic target 不在 Profile allowlist；
- Provider maturity 组合既不是 ProductionVerified，也不是 HardwareCampaign。

关于最后一条：ProductionVerified 的定义是「这个组合已通过真机验收」，
而真机验收本身需要一个 executable plan。若只承认 ProductionVerified，
任何新组合的**第一次**刷写都不可达——门不是严，是死。

HardwareCampaign 是那次验收本身，不是对门的放宽：

- 必须由操作员具名开启(`arkforged --hardware-campaign <id>`)，缺省仍是 HardwareGated；
- transcript replay 永远不适用，PlanOnly 的理由与 AF-V1 相同——录像不是设备；
- 它进 plan 封印，因此 campaign 计划与 production 计划摘要必然不同。
  StepPermit 只绑 plan digest，这条保证一次验收活动的 permit 与回执
  无法被当作「该组合已受支持」的证据重放。

见 `docs/decisions/AFD-0004-hardware-campaign-maturity.md`。

---

## 6. 公开计划与私有执行计划的完整性

### 6.1 两个计划

ArkForge 同时维护：

1. Public Plan：供 ArkDeck 展示、验证、授权和审计；
2. Private Execution Plan：Provider 实际使用的地址、工具动作、packet、FDL 参数和 parser policy。

Private Plan 不通过 Agent/App API 暴露，但必须被授权摘要覆盖。

### 6.2 必需摘要

~~~text
privateActionDigest[i] =
  SHA256(
    domain ||
    canonical_private_action_i
  )

providerExecutionPlanDigest =
  SHA256(
    domain ||
    ordered(privateActionDigest[])
  )

publicProjectionDigest =
  SHA256(
    domain ||
    ordered(publicStep ↔ privateActionDigest mapping)
  )

planDigest =
  SHA256(
    domain ||
    deterministic_cbor(
      plan metadata +
      public steps +
      effect set +
      providerExecutionPlanDigest +
      publicProjectionDigest
    )
  )
~~~

### 6.3 Projection invariant

每个 private action 必须映射到恰好一个 public step，或被声明为该 public step 内的 read-only transport sub-action。

禁止：

- 一个 private destructive action 没有 public effect；
- private address 超出 public semantic target；
- private artifact slice 与 public content digest 不一致；
- Provider 在 start 后重新 lowering；
- daemon 升级后用新 Provider 解释旧 private plan；
- store corruption 后只校验 public plan。

### 6.4 每步执行检查

执行 ACT-003 时，ArkForge 必须同时校验：

~~~text
stored planDigest
stored providerExecutionPlanDigest
stored privateActionDigest(ACT-003)
StepPermit.privateActionDigest
Provider implementation digest
Profile digest
Artifact digest
Toolchain digest
fresh device facts digest
~~~

任一变化都在外部 effect 前 fail closed。

---

## 7. ArkDeck Operation 与 ArkForge API 边界

### 7.1 Agent/App 可提交的内容

Agent/App 只能提交语义 Operation：

~~~text
operationReference
targetReference
artifactLease
typed intent
verification preference
bounded budget
~~~

不能提交：

- planID；
- planDigest；
- Provider；
- partition/address；
- tool；
- raw options；
- private action；
- recovery classification。

### 7.2 推荐的 ArkDeck Operation

推荐新增：

~~~text
flash.full-restore@1
~~~

其 typed input 只表达：

~~~yaml
artifactLease: required
deviceProfileRef: derived-from-bound-target-or-explicit-published-ref
intent: fullRestore
verification: basic | full
~~~

Catalog 与发布 profile 必须共同限制：

- permitted DeviceProfile；
- permitted Provider combinations；
- allowed WorkflowStep kinds；
- effect ceiling；
- maximum actions/partitions；
- userdata policy；
- complete-overwrite recovery contract；
- availability。

现有 flash.dayu200 保留一个兼容周期，并在内部映射到同一 generic ArkForge adapter。兼容 alias 不能保留 Rockchip lowering。

DAYU600 发布前保持 profile/operation unavailable。不得因为 ArkForge 能 inspect PAC 就把 execute 标记可用。

### 7.3 内部 API

planID 和 planDigest 只存在于：

~~~text
ArkDeck Runtime ↔ arkforge-arkdeck-adapter ↔ arkforged
~~~

它们不是公开 Catalog input。

### 7.4 新 Operation 的审批

flash.full-restore@1 是新 Operation，ArkForge executor 是新 Provider/integration，DAYU600 是新 device profile。它们属于 ArkDeck 明确要求 review 的变更类型，必须与对应真实产品能力同车交付。

---

## 8. ExecutionAuthority 与 StepPermit

### 8.1 中性接口

~~~rust
#[async_trait]
pub trait ExecutionAuthority: Send + Sync {
    async fn request_step_permit(
        &self,
        request: StepAdmissionRequest,
    ) -> Result<StepPermitDecision, AuthorityError>;

    async fn acknowledge_receipt(
        &self,
        receipt: ActionReceiptSummary,
    ) -> Result<(), AuthorityError>;
}
~~~

Provider 不能调用或解释 Authority。只有 Engine 能请求 permit。

### 8.2 StepAdmissionRequest

~~~rust
pub struct StepAdmissionRequest {
    pub request_id: RequestId,
    pub controller_session_id: ControllerSessionId,
    pub job_id: JobId,
    pub plan_id: PlanId,
    pub plan_digest: Sha256,
    pub step_id: StepId,
    pub attempt_id: AttemptId,
    pub public_step_digest: Sha256,
    pub private_action_digest: Sha256,
    pub effect_set_digest: Sha256,
    pub authority_binding: AuthorityBindingRef,
    pub admission_snapshot: StepAdmissionSnapshot,
    pub requested_at_epoch_ms: u64,
}
~~~

### 8.3 StepAdmissionSnapshot

ArkForge 在申请 permit 前读取：

- current USB/protocol identity；
- mode；
- topology；
- loader/agent stage；
- Provider/Profile；
- artifact/toolchain；
- open session/handle identity；
- power/driver/tool facts；
- previous semantic receipt；
- pending cancellation；
- local executor lane。

这些事实形成：

~~~rust
pub struct StepAdmissionSnapshot {
    pub captured_at_epoch_ms: u64,
    pub freshness_deadline_epoch_ms: u64,
    pub device_facts_digest: Sha256,
    pub transport_session_digest: Option<Sha256>,
    pub provider_facts_digest: Sha256,
    pub toolchain_facts_digest: Sha256,
    pub artifact_facts_digest: Sha256,
}
~~~

Freshness 判定以连续性事实为主，墙钟为兜底：

- 主事实：同一 open session/handle 连续(`transport_session_digest` 不变)且自 snapshot 起未观测到 detach/re-enumeration；
- `freshness_deadline_epoch_ms` 是兜底上限，覆盖 snapshot → ArkDeck 重验证 → permit → dispatch 的完整往返，必须按 step kind 与宿主负载预算，不使用全局短常量(仓内定案教训：对真实工作设固定短墙钟预算是一族 flake 来源，本机与 CI 都会真红，参 PR #1008/#1080)；
- 墙钟过期而连续性事实完好时，判定为 stale snapshot：重新 snapshot、重新申请 permit，不判设备故障，不消耗 destructive 预算。

### 8.4 StepPermit

~~~rust
pub struct StepPermit {
    pub permit_id: PermitId,
    pub authority_namespace: AuthorityNamespace,
    pub controller_session_id: ControllerSessionId,
    pub job_id: JobId,
    pub plan_id: PlanId,
    pub plan_digest: Sha256,
    pub step_id: StepId,
    pub attempt_id: AttemptId,
    pub public_step_digest: Sha256,
    pub private_action_digest: Sha256,
    pub effect_set_digest: Sha256,
    pub authority_binding: AuthorityBindingRef,
    pub admitted_device_facts_digest: Sha256,
    pub issued_at_epoch_ms: u64,
    pub expires_at_epoch_ms: u64,
    pub single_use: bool,
    pub integrity_tag: PermitIntegrityTag,
}
~~~

### 8.5 Permit 执行规则

1. ArkDeck 收到 admission request；
2. ArkDeck 重新验证 RuntimeCapability、target binding、Artifact、plan、tool facts 和预算；
3. ArkDeck durable 写 Runtime intent；
4. ArkDeck 返回 StepPermit；
5. ArkForge durable 写 PermitAccepted 和 StepIntent；
6. ArkForge 在同一 session/handle 上重新读取 identity；
7. 若 facts digest 漂移或 freshness 过期，零 dispatch，permit 标记未消费；
8. facts 一致后，ArkForge durable 写 PermitConsuming；
9. 执行 exact private action；
10. durable 写 raw evidence、semantic receipt 和 PermitConsumed；
11. ArkDeck 观察 receipt 并结清 exact Runtime intent。

同一个 permit 可以在 IPC 丢包时重新传输，但：

- permitID 不变；
- 不能生成第二个 StepIntent；
- 不能创建第二个 dispatch；
- 已消费 permit 只能返回原 receipt；
- expiry 后不能首次消费。

### 8.6 Permit 完整性与重传信任模型

`integrity_tag` 的信任链必须显式：

- 铸造方唯一：tag 由 ArkDeck Runtime 侧(经 adapter)以 controller pairing secret 铸造；secret 即 15.2 中 daemon 启动时从 ArkDeck 继承的 handle/secret，仅存内存，不落盘明文；
- arkforged 只验证、不铸造；验证失败是授权边界事件：零 dispatch、journal 记录、fail closed；
- 重传语义：ArkDeck 在返回 permit 前 durable 持久化完整 permit(含 tag)，IPC 丢包重传即重放存储副本；禁止确定性重派生，避免出现两份字节不同的「同一」permit；
- 重启语义：任一侧 daemon 重启后 pairing secret 轮换，旧 secret 背书的未消费 permit 不可首次消费，只能作废并由 ArkDeck 重新走 admission；已消费者照常返回 durable receipt；
- permitID 全局唯一且 single_use，消费状态以 ArkForge journal 的 PermitConsuming/PermitConsumed 记录为准。

### 8.7 Lane 所有权

ArkDeck Runtime 的 device-exclusive lane 是唯一 authority lane。

ArkForge 可维护 local executor lease，用于防止 daemon 内部并发 bug，但它：

- 不创建执行权；
- 不替代 Runtime reservation；
- 不独立解除 ArkDeck lane；
- 不在 ArkDeck unresolved intent 时接收另一个 controller 的 job。

---

## 9. HDC 与设备模式转换

### 9.1 原则

ArkForge 决定需要进入哪个 semantic mode，但不直接管理 ArkDeck 的 HDC server。

ArkDeck 继续拥有：

- HDC endpoint；
- server ownership；
- start/stop/restart protection；
- exact connectKey；
- target binding；
- device-scoped -t 绑定；
- server journal。

### 9.2 ManagedDeviceControlPort

~~~rust
#[async_trait]
pub trait ManagedDeviceControlPort {
    async fn execute(
        &self,
        action: ManagedDeviceControlAction,
        target: AuthorityBindingRef,
        permit: VerifiedStepPermit,
    ) -> Result<ManagedControlReceipt, ManagedControlError>;
}

pub enum ManagedDeviceControlAction {
    EnterUpdater,
    RebootToNormal,
    ReadProductFacts,
    ReadBuildFacts,
}
~~~

ArkDeck adapter 负责把这些动作映射到已有 typed HDC Provider。

ArkForge 不接收：

- HDC executable path；
- connectKey string override；
- raw HDC argv；
- shell；
- server lifecycle action。

### 9.3 非 HDC 进入模式

如果设备只能通过按键、短接或断电进入下载模式，ArkForge 返回 typed HumanActionRequired：

~~~text
required physical action
expected next mode
identity/rebind expectation
deadline
data impact
~~~

人工动作不是 authority，也不能降低后续 exact identity 检查。

---

## 10. Artifact 系统

### 10.1 Artifact 导入

ArkForge 不接受 destructive plan 使用任意 host path。

推荐：

1. ArkDeck 持有 immutable Artifact lease；
2. Unix 使用 read-only FD transfer，Windows 使用只读 HANDLE 或 controller stream；
3. ArkForge 计算 size/hash；
4. 导入 content-addressed store；
5. 返回 artifactID；
6. plan/start 只引用 artifactID 和 digest。

首版若 handle transfer 复杂，可以使用 controller-only streaming。不能退化成 daemon 重新打开 caller path。

### 10.2 CAS 生命周期

必须定义：

- import quota；
- available-space preflight；
- active plan/job lease；
- reference counting；
- crash-safe GC；
- max retained evidence；
- sensitive artifact encryption/ACL；
- export audit；
- daemon/App 升级兼容；
- 大文件复制成本。

大文件成本的基线答案：同卷导入优先 APFS clonefile/`copy_file_range`，字节复制近零，内容哈希仍需全量流式读；跨卷与流式导入以仓内实测 ~10GB/min(#1003 修复后的 RPC 导镜像锚点)为参照。AF-V1 验收对 DAYU200 整包导入给出明确预算，并实测 available-space preflight。

### 10.3 Parser 边界

Parser：

- 无 USB；
- 无网络；
- 无进程执行；
- 不决定 authority；
- 不生成 raw vendor options；
- 只输出事实、unknown 和 confidence。

### 10.4 DAYU200 Artifact

DAYU200 parser 必须：

- 流式读取 gzip/tar；
- 拒绝绝对路径、..、symlink、hardlink、device node；
- 计算 archive 和 member hash；
- 解析 parameter.txt；
- 验证 partition overlap/bounds；
- 识别 MiniLoaderAll、mapped image 和未知 member；
- 从 system image 或其他受验证来源提取 build facts；
- 不以文件名猜测版本；
- 不在 parser 内写死九分区 allowlist。

九分区允许集属于 DAYU200 DeviceProfile。

### 10.5 DAYU600 PAC

PAC parser 分两类输出：

#### Research inspection

- 识别 header/table/segment candidate；
- 记录 offset/length/hash；
- unknown execution field 显式保留；
- confidence = ResearchOnly；
- 只能返回 PlanAssessment。

#### Production manifest

只有证据门全部通过后才能输出：

- format/version；
- signature/checksum；
- FDL/boot agent；
- load address/entry/security；
- partition/address/length；
- erase policy；
- write order/dependency；
- verify algorithm；
- userdata/NV/calibration/security impact；
- execution-relevant unknowns = empty。

---

## 11. Transport 与设备身份

### 11.1 Transport API

~~~rust
#[async_trait]
pub trait DeviceTransport {
    async fn discover(
        &self,
        filter: &TypedDiscoveryFilter,
        deadline: Deadline,
    ) -> Result<Vec<DeviceObservation>, TransportError>;

    async fn open_exact(
        &self,
        observation: &DeviceObservation,
        policy: &IdentityEvidencePolicy,
    ) -> Result<Box<dyn TransportSession>, TransportError>;

    async fn wait_for_rebind(
        &self,
        expectation: &RebindExpectation,
        previous: &DeviceObservation,
        deadline: Deadline,
    ) -> Result<DeviceObservation, TransportError>;
}
~~~

Typed USB request 只能由具体 protocol module 构造，不能从 IPC 反序列化任意 setup packet。

### 11.2 DeviceObservation

~~~rust
pub struct DeviceObservation {
    pub observation_id: ObservationId,
    pub observed_at_epoch_ms: u64,
    pub mode: DeviceMode,
    pub topology_digest: Sha256,
    pub descriptor_digest: Sha256,
    pub serial_evidence: SerialEvidence,
    pub protocol_identity: Vec<ProtocolIdentityFact>,
    pub provider_candidates: Vec<ProviderCandidate>,
    pub identity_strength: IdentityEvidenceStrength,
}
~~~

VID/PID 可以存在于 ArkForge Profile/Transport 内部，但不能单独构成 stable target。

### 11.3 Rebind

RebindExpectation 必须包含：

- from/to mode；
- disconnect 是否必须；
- allowed identity set digest；
- serial policy；
- topology policy；
- protocol identity query；
- uniqueness；
- deadline；
- identity strength floor。

出现以下情况必须停止：

- 0 个候选；
- 多个候选；
- identity strength 下降；
- topology/serial/protocol ID 不满足 policy；
- 设备被替换；
- expected mode 未出现。

不得选择 first match。

RebindExpectation 同时必须携带显式的瞬态容忍策略。真机重枚举过程存在瞬态 malformed descriptor 与过渡态观测(DAYU200 实证：#1067 preflight 须认 normal 别名、#1068 wait 须容忍瞬态 malformed)：

- tolerance window 内的 malformed/过渡观测记为 evidence，不判 fatal；窗口耗尽仍未达 expected mode 才停止；
- 模式别名等价关系(如 normal 的别名族)是 DeviceProfile 声明的事实，不是 Transport 实现的即兴判断；
- identity strength 与 mode 的终局比较只在稳定观测之间进行，不以瞬态观测触发 downgrade 判定。

### 11.4 Transcript

默认 transcript 只记录：

- request/response 类型；
- 长度；
- payload hash；
- parsed semantic fields；
- status；
- timing；
- attach/detach/rebind；
- tool exit 与输出 evidence hash。

完整固件 payload 只进入单独授权的加密研究证据库。

---

## 12. Provider SPI

### 12.1 生命周期

~~~text
probe
  → validate
  → materialize
  → start
  → request permit
  → execute exact stored action
  → semantic receipt
  → checkpoint
  → postflight
  → result

failure
  → read-only reconcile
  → possible effect assessment
  → optional superseding recovery plan
  → recovery guide
~~~

### 12.2 SPI 草案

~~~rust
#[async_trait]
pub trait FlashProvider: Send + Sync {
    fn descriptor(&self) -> ProviderDescriptor;

    async fn probe(
        &self,
        ctx: &ProbeContext<'_>,
    ) -> Result<ProviderProbe, ProviderError>;

    fn validate(
        &self,
        artifact: &ArtifactManifest,
        profile: &DeviceProfile,
        probe: &ProviderProbe,
    ) -> Result<ValidationReport, ProviderError>;

    fn materialize(
        &self,
        artifact: &ArtifactManifest,
        profile: &DeviceProfile,
        probe: &ProviderProbe,
        intent: &FlashIntent,
    ) -> Result<PlanMaterialization, ProviderError>;

    async fn execute_stored_action(
        &self,
        ctx: &ExecutionContext<'_>,
        stored_plan: &StoredProviderPlan,
        action_id: &ActionId,
        verified_permit: &VerifiedStepPermit,
    ) -> Result<ActionReceipt, ProviderError>;

    async fn postflight(
        &self,
        ctx: &PostflightContext<'_>,
        stored_plan: &StoredProviderPlan,
    ) -> Result<PostflightReceipt, ProviderError>;

    async fn reconcile_read_only(
        &self,
        ctx: &ReconcileContext<'_>,
        unresolved: &UnresolvedAction,
    ) -> Result<ReconcileAssessment, ProviderError>;

    fn possible_effects(
        &self,
        unresolved: &UnresolvedAction,
    ) -> Result<PossibleEffectSet, ProviderError>;

    fn materialize_superseding_recovery(
        &self,
        ctx: &RecoveryPlanContext<'_>,
        uncertain_effects: &PossibleEffectSet,
    ) -> Result<SupersedingRecoveryAssessment, ProviderError>;

    fn recovery_guide(
        &self,
        ctx: &RecoveryContext<'_>,
    ) -> Result<RecoveryGuide, ProviderError>;
}
~~~

### 12.3 Provider maturity

Maturity 不是 Provider 全局字段，而是组合键：

~~~text
provider
+ provider implementation digest
+ profile/version/digest
+ artifact format/version
+ toolchain/backend digest
+ host platform
+ driver facts
+ evidence set
~~~

状态：

- ProductionVerified；
- HardwareCampaign(campaign)——正在进行的具名真机验收，可支撑 executable plan，
  但不是 production evidence(见 5.5)；
- HardwareGated；
- PlanOnly；
- ResearchOnly；
- Unavailable(reason)。

同一个 Unisoc Provider 可以对某一 SoC/Profile 为 ProductionVerified，对 DAYU600 仍为 PlanOnly。

### 12.4 Semantic receipt

ActionReceipt 至少包含：

- job/plan/step/action/attempt/permit ID；
- plan/public/private action digest；
- provider/profile/toolchain；
- device before/after facts；
- transport/tool/protocol status；
- semantic assertions；
- observed effects；
- raw evidence refs；
- possible effect set；
- disposition；
- receipt digest。

exit 0 不单独等于 success。

---

## 13. Durable Engine

### 13.1 状态机

~~~mermaid
stateDiagram-v2
    [*] --> Planned
    Planned --> AwaitingStart
    AwaitingStart --> Preflight
    Preflight --> AwaitingPermit
    Preflight --> ReadOnlyDispatch

    AwaitingPermit --> StepIntentDurable
    AwaitingPermit --> CancelledSafe

    StepIntentDurable --> Dispatching
    Dispatching --> ReceiptDurable
    ReceiptDurable --> Checkpointed

    Checkpointed --> RebindWait
    RebindWait --> Preflight
    Checkpointed --> Preflight
    Checkpointed --> Postflight

    Postflight --> Succeeded
    Postflight --> ConfirmedFailed

    StepIntentDurable --> OutcomeUnknown
    Dispatching --> OutcomeUnknown
    ReceiptDurable --> OutcomeUnknown
    RebindWait --> OutcomeUnknown

    OutcomeUnknown --> Reconciling
    Reconciling --> OutcomeUnknown
    Reconciling --> Succeeded
    Reconciling --> ConfirmedFailed
    OutcomeUnknown --> RecoveryAssessable

    RecoveryAssessable --> [*]
    Succeeded --> [*]
    ConfirmedFailed --> [*]
    CancelledSafe --> [*]
~~~

RecoveryAssessable 不是原 Job 的成功状态。它仅表示 ArkForge 能提供一个可能的 distinct recovery plan，最终是否准入由 ArkDeck 决定。

### 13.2 ArkForge journal

至少包含：

~~~text
PlanStored
JobCreated
LocalExecutorLeaseAcquired
PreflightObserved
StepAdmissionRequested
StepPermitAccepted
StepIntentRecorded
PermitConsuming
ExternalDispatchStarted
TransportEvidenceRecorded
SemanticReceiptRecorded
PermitConsumed
StepCheckpointed
RebindObserved
CancellationRequested
PostflightRecorded
OutcomeClassified
PossibleEffectSetRecorded
RecoveryAssessmentPublished
RecoveryGuidePublished
~~~

每条记录包含：

- schema version；
- sequence；
- previous digest；
- record digest；
- fsync policy；
- job revision。

fsync policy 是**记录种类的函数，不是可调项**。凡是丢失后会让 ArkForge 二次派发、
或忘记自己已经派发的记录，一律 durable；其余（PreflightObserved、
StepAdmissionRequested、TransportEvidenceRecorded、RebindObserved、
ReadOnlyObservationRecorded）为 buffered——丢失只损失记录里的细节，不改变任何判定。
声明了比自身种类更弱策略的记录按篡改处理。

#### 13.2.1 durability 的边界（AD-017）

本设计的 durability 只声明到**进程死亡**为止：

- 与派发决定相关的记录在 `append` 返回前 `fsync`，因此「返回了」意味着已落稳定存储；
- 撕裂尾部（崩溃打断了一次写）在重放时要么作为更短的前缀被接受，要么被拒绝，
  不存在「静默丢一条中间记录」的第三种结果；这一条由穷举每一个可能撕裂位点的
  测试保证，不是靠推理。

**不声明掉电安全。** macOS `fsync(2)` 不冲刷驱动器自身的写缓存——那需要
`F_FULLFSYNC`，而它要 libc，AFD-0001 不允许本仓引入。因此掉电语义是**未验证**的，
记为 open 边界而非已通过的门。任何上层（含 authority）不得据此声称掉电安全。

### 13.3 Crash 语义

| Crash 窗口 | 处理 |
|---|---|
| start 前 | 无 job，可重新 plan/start |
| permit 前 | 安全暂停，可取消 |
| ArkDeck intent 已落、permit 传输丢失 | 重传同 permitID，不能创建第二个 intent |
| permit 收到、ArkForge StepIntent 未落 | 禁止 dispatch；恢复后落同 intent或过期停止 |
| StepIntent 已落、dispatch 是否发生不明 | outcomeUnknown |
| dispatch 后无 semantic receipt | outcomeUnknown |
| receipt durable、checkpoint 未落 | 验证 exact receipt 后补 checkpoint，不重执行 |
| checkpoint 已落、ArkDeck 未观察 | event replay，不重执行 |
| postflight 前崩溃 | 只读 postflight/reconcile |

### 13.4 Cancel

- read-only：尽快取消；
- permit 前：CancelledSafe；
- mode transition dispatch 后：等待 rebind/reconcile；
- critical write：排队到 safe boundary；
- unresolved intent 存在时不能返回 CancelledSafe；
- kill tool 不等于安全 cancel；
- protocol 未证明可原子取消时不能中断 transport。

---

## 14. OutcomeUnknown 与完整覆盖恢复

### 14.1 永不 replay

以下永远禁止：

- 重发原 intent；
- 重用原 permit；
- 把同 plan 的 start 当重试；
- 修改原 outcomeUnknown；
- 因用户确认而绕过 missing proof；
- 把完整重刷改名为 retry。

### 14.2 Read-only reconcile

reconcileJob：

- 不申请 mutation/destructive permit；
- 不发送 erase/write/load-agent/reboot；
- 只执行 Profile 声明的 read-only observation；
- 可以证明 succeeded、confirmed not executed、confirmed partial 或仍 unknown；
- 证据不足时保持 outcomeUnknown。

### 14.3 PossibleEffectSet

每个 unresolved action 必须映射保守 possible effects：

~~~rust
pub struct PossibleEffectSet {
    pub effects: Vec<PossibleEffect>,
    pub completeness: EffectSetCompleteness,
    pub source_intents: Vec<IntentRef>,
    pub digest: Sha256,
}
~~~

可选/条件 effect 默认包含在 union，除非 durable evidence 证明未发生。

无法界定 effect 时：

~~~text
completeness = Unbounded
recovery eligibility = false
~~~

### 14.4 Provider RecoveryCoverageDeclaration

Provider 可对 exact 组合声明：

~~~text
operation/version
profile/version
provider/backend/toolchain
closed mutable effect vocabulary
intent → possible effect mapping
complete overwrite plan recipe
per-effect verification
reboot/rebind/postflight
unsupported states
~~~

该声明是 code-reviewed/published policy，不是 caller flag。

### 14.5 SupersedingRecoveryAssessment

ArkForge 接收 ArkDeck 提供的 uncertainEffectSet digest 和 fresh mechanics facts，返回：

~~~rust
pub enum SupersedingRecoveryAssessment {
    Eligible(SupersedingRecoveryPlan),
    Ineligible(RecoveryBlocker),
}
~~~

SupersedingRecoveryPlan 必须：

- 是新的 planID；
- 有新的 planDigest；
- 显式声明 executionPurpose = SupersedingRecovery；
- 覆盖 uncertainEffectSet；
- 覆盖 partition、boot metadata、userdata、mode 和其他 provider effect；
- 包含逐项 verification；
- 包含 reboot/rebind/runtime-build postflight；
- 不引用原 permit 或 intent。

### 14.6 ArkDeck 最终准入

只有 ArkDeck Runtime 可以：

1. union 所有 outstanding uncertain effects；
2. 验证 same target、binding、topology、artifact、tool facts；
3. 检查 Provider published coverage；
4. 检查 16 epoch、四小时、并发一预算；
5. durable 分类 safeToSupersedeByCompleteOverwrite；
6. 创建新的 RuntimeCapability/reservation/intent；
7. 启动 distinct recovery epoch；
8. 在完整成功后写 SupersedingRecoveryEpoch。

原 Job 和原 outcomeUnknown 永远保持不变。

---

## 15. IPC 与进程模型

### 15.1 进程

- arkdeck-agentd：ArkDeck Runtime、authority、target binding 和 Runtime journal；
- arkforged：mechanics、Provider、Transport、operational journal；
- parser worker：短生命周期、最低权限；
- ArkDeck App：不直接访问 USB/vendor tool；
- arkforge-cli：v1 默认仅 inspect/probe/assessment/diagnostics。

### 15.2 IPC

- macOS/Linux：Unix Domain Socket；
- Windows：local-only Named Pipe；
- Protobuf major/minor negotiation；
- frame size limit；
- request ID；
- stream sequence；
- controller/public session 分离；
- unknown enum fail closed；
- destructive controller 优先使用 ArkDeck 启动 daemon 时继承的 handle/secret；
- public socket 不提供 startExecution。

Windows Named Pipe 为设计预留：ArkDeck 唯一生产平台是 macOS，Windows 传输面不进入 AF-V1/AF-V2 验收范围，待真实 Windows 产品需求出现时按 maturity 组合键单独验收。

### 15.3 API

| API | 权限 | 结果 |
|---|---|---|
| importArtifact | controller | artifactID/hash |
| inspectArtifact | read-only/controller | ArtifactManifest |
| discoverDevices | read-only/controller | DeviceObservation |
| probeDevice | read-only/controller | candidates/capabilities |
| materializePlan | controller；CLI 仅 assessment | Executable plan 或 PlanAssessment |
| startExecution | controller only | jobID |
| watchJob | authorized observer | ordered events |
| cancelJob | controller | cancellation state |
| reconcileJob | controller | read-only assessment |
| planSupersedingRecovery | controller | eligible/ineligible assessment |
| getRecoveryGuide | authorized observer | typed guide |

startExecution 只接受内部：

~~~text
planID
planDigest
executionPurpose
controller session
~~~

不接受 partition、address、tool、FDL、timeout 或 effect override。

### 15.4 Digest

Protobuf 负责通信兼容，deterministic CBOR 负责安全摘要。

计划摘要必须覆盖：

- authority binding；
- provider/profile/artifact/toolchain；
- capabilities；
- ordered public steps；
- complete EffectSet；
- per-action private digest；
- provider execution plan digest；
- cancellation/timeouts/rebind/postflight；
- recovery contract；
- expiry；
- execution purpose。

禁止 float、host path、locale 文案和不规范 ID 进入 digest model。

---

## 16. DAYU200 垂直设计

### 16.1 Provider 路线

首版：

- 使用当前已经真机验证的 rkdeveloptool；
- 固定 executable hash/version/signature/ACL；
- 直接 Process API spawn；
- 无 shell；
- 无 PATH 解析；
- 无 caller argv；
- Provider 内部 closed enum lowering；
- stdout/stderr parser 与 tool digest 绑定。

后续原生 RockUSB：

- 只有协议 transcript、错误语义和 fault matrix 足够后才启用；
- public plan/effect/result 不变化；
- backend digest 变化必须重新 plan/authorize。

### 16.2 动作映射

| Public step | Private action | 语义成功 |
|---|---|---|
| EnsureMode | EnterLoader through ManagedDeviceControlPort | HDC accepted + expected disconnect + unique Loader rebind |
| ProbeDevice | ProbeLoader | exact profile/mode/identity evidence |
| ValidateLayout | ValidatePartitionTable | observed layout digest matches |
| WriteTarget | WritePartition | exact target/content + completion semantics |
| VerifyTarget | CharacterizeReadDomain + ReadbackPartition | 读域覆盖时 declared range/hash verified；读域不覆盖时 typed skip 进入 receipt(见 16.4)，不判失败 |
| Reboot | ResetDevice | reset semantics + disconnect |
| PostflightProbe | VerifyHDCPostflight | exact target re-adopted + model/build match |

### 16.3 分区与数据影响

当前 DAYU200 Profile 的九个写入：

| 顺序 | 分区 | 起始 LBA | 数据影响 |
|---:|---|---:|---|
| 1 | uboot | 8192 | bootloader |
| 2 | resource | 28672 | resource |
| 3 | boot_linux | 40960 | boot image |
| 4 | ramdisk | 237568 | ramdisk |
| 5 | system | 245760 | system |
| 6 | vendor | 4440064 | vendor |
| 7 | updater | 6742016 | updater |
| 8 | chip_ckm | 6938624 | chip_ckm |
| 9 | userdata | 19955712 | 覆盖用户数据 |

Artifact 出现 image 不代表允许写。Profile allowlist、observed partition table 和 artifact manifest 必须三方一致。

### 16.4 Verification 与读域硬事实

DAYU200 板端 RockUSB 的读写面不对称，是 GJ-4 真机 campaign 的定案事实(ECAMP-96EFFF15 / ECAMP-31E041BC，修复链 PR #1066–#1070，见 AD-006)：

- LBA 读面(`rl`)存在读窗：2026-08-04 实测自扇区 65536(32 MiB)起结构性盲区，窗口外读取恒返回 uniform filler(0xCC)，与介质真实内容无关；
- 擦除介质在可读域内同样读为 uniform 0xCC；
- 写面(`wlx`)全盘可达——短读窗不意味着短写窗；
- 当日全部「假写」证据均为窗口外 `rl` 读取的冤案，写入实际已落盘并可启动。

因此 readback 验证必须先 characterize read domain(现行生产实现即如此：`RockchipRuntimeActionHost.characterizeMediumReadDomain`，windowed 时 readback 显式跳过并在 receipt 记录 `skipped-lba-read-window` 与 `readDomainDetail`)。读窗大小是运行时实测事实，不是 Profile 常量；Profile 只声明「读面须实测」与 erased-medium filler 语义。验证结果是三态：

- Verified：读域覆盖 declared range 且 hash 一致；
- TypedSkip：读域不覆盖，receipt 记录跳过范围与原因；不判失败，也不得冒充任何 verified 强度；
- Failed：读域覆盖但内容不一致；uniform 0xCC 的语义是「未写入/擦除介质/窗口外」，必须单列判定，不得直接判 hash mismatch。

验证强度必须显式：

- FullHash；
- SampledRanges；
- PrefixHash；
- SemanticOnly。

PrefixHash 不能在结果中写成 full verified；TypedSkip 不计入任何 verified 强度。九分区中绝大多数目标(system 起始 LBA 245760、vendor 4440064 等)远在读窗之外，其实际验证依赖 `wlx` 完成语义 + reboot 后 build postflight；Profile 的 per-target 验证声明必须如实表达这一点，不得声明读域外的 readback 强度。

Postflight 至少验证：

- exact target lineage；
- normal/HDC mode；
- product model；
- runtime build 与 artifact manifest；
- required HDC facts；
- Profile 声明的 boot/layout facts。

### 16.5 迁移 parity

新路径必须对照旧路径：

- artifact/member hashes；
- partition/address/order；
- userdata impact；
- mode transition；
- tool identity；
- semantic markers；
- read domain characterization 与 readback/typed-skip coverage；
- reboot/rebind；
- build postflight；
- complete-overwrite recovery；
- crash/cancel/outcomeUnknown。

任何 external intent 后不能切回 legacy backend。

Parity 的锚是真实 campaign 收据：GJ-4 两次真机 campaign(ECAMP-96EFFF15 / ECAMP-31E041BC)的 13 步 receipts 应作为 golden transcript 进入 `transcripts/`，Provider contract 与 Transport replay 测试自首日对其回放。

---

## 17. DAYU600 研究与设计

### 17.1 当前结论

确认：

- DAYU600 与 uis7885/UNISOC/PAC 路径相关；
- 与 DAYU200 RockUSB 不同；
- 应新增 Unisoc Provider 和 DAYU600 DeviceProfile。

仍未知：

- PAC version/schema；
- FDL stage、地址和安全握手；
- Download USB request/ACK/error；
- exact mode identity；
- storage/partition/erase/write/verify；
- cancel/recovery；
- 跨平台 driver；
- 厂商工具许可和再分发。

因此：

~~~text
inspect = allowed
discover/probe = allowed when read-only evidence exists
PlanAssessment = allowed
Executable FlashPlan = forbidden
startExecution = unavailable
~~~

### 17.2 PAC 研究方法

1. 获取多个可信版本 PAC；
2. 记录来源、hash、官方工具和设备版本；
3. 只读结构扫描；
4. 多样本差分；
5. 隔离 Windows VM 观察官方工具；
6. 合法授权 USB capture；
7. 关联 PAC segment、FDL、USB mode、storage action；
8. 与设备分区/booted facts 交叉验证；
9. 独立 parser 交叉验证；
10. fuzz header/table/segment/compression/checksum。

### 17.3 必须取得的真机事实

- normal mode descriptor/HDC identity；
- Download/BootROM mode；
- FDL1/FDL2 mode；
- VID/PID/interface/endpoint；
- serial/chip unique ID；
- topology；
- re-enumeration；
- 双设备唯一选择；
- Windows driver；
- macOS/Linux claim；
- error、timeout、disconnect、cancel。

### 17.4 三方一致原则

执行相关字段必须满足：

~~~text
PAC parser observation
  ==
official tool/log behavior
  ==
USB transcript or device postflight fact
~~~

单独一个来源不能开启 destructive execute。

### 17.5 证据门

DAYU600 ProductionVerified 前必须全部 PASS：

1. PAC format/version；
2. signature/checksum；
3. FDL identity/address/order/security；
4. exact USB identity；
5. stable chip/device identity；
6. request/ACK/error/timeout；
7. storage/erase/write/verify/reboot；
8. 每个 destructive step 的断连结果；
9. possible effect mapping；
10. read-only reconcile；
11. complete-overwrite coverage 或明确不支持；
12. driver/platform acceptance；
13. license/redistribution；
14. parser fuzz；
15. provider/transcript contract；
16. real DAYU600 acceptance；
17. ArkDeck review；
18. 无 force/experimental bypass。

---

## 18. DeviceProfile

### 18.1 原则

DeviceProfile 描述：

- product/SoC/board identity；
- hardware revision allowlist；
- provider combinations；
- artifact compatibility；
- mode identity；
- mode transition；
- storage/layout；
- read domain(读写面可达性与 erased-medium 语义)；
- allowed/protected targets；
- data impact；
- verification(per-target 可达强度)；
- recovery coverage。

生产 Profile 不使用 hardware revision wildcard，除非 accepted evidence 明确证明 revision-independent，并在 Profile 中引用该证据。

### 18.2 示例骨架

~~~yaml
schemaVersion: arkforge.device-profile/v1
profile:
  id: org.openharmony.dayu200
  version: 1.0.0
  digest: sha256:...
identity:
  productModels: [DAYU200]
  soc:
    vendor: rockchip
    family: rk3568
  hardwareRevisions:
    allow: [verified-revision-id]
providers:
  - id: arkforge.rockchip
    backend: rkdeveloptool-fixed
    versionRange: ">=1.0.0 <2.0.0"
artifactCompatibility:
  formats: [rockchip-images-targz]
modes:
  - id: hdc-normal
  - id: rockusb-loader
modeTransitions:
  - from: hdc-normal
    to: rockusb-loader
    action: enter-updater
storage:
  kind: emmc
  logicalBlockSize: 512
readDomain:
  write: full-disk               # wlx 全盘可达(AD-006)
  read: characterize-at-runtime  # rl 读窗须每次执行实测；2026-08-04 观测为扇区 65536 起盲区
  erasedMediumFiller: 0xCC
allowedTargets: []
protectedTargets: []
dataImpact:
  userdata: overwritten
verification:
  perTarget:
    default:
      strategy: readback-if-read-domain-covers
      fallback: typed-skip + wlx-completion-semantics + build-postflight
recovery: {}
evidenceRefs: [AD-006]
~~~

### 18.3 Profile invariant

- no wildcard write range；
- unknown userdata impact blocks execute；
- unknown hardware revision blocks execute；
- Profile digest enters plan；
- Profile update requires new plan；
- protected target cannot be overridden；
- provider maturity is combination-scoped；
- verification 声明不得超出 readDomain 可达范围，TypedSkip 不计为 verified；
- 模式别名与瞬态容忍策略是 Profile 声明的事实；
- recovery declaration is versioned and exact。

---

## 19. 测试与故障注入

### 19.1 测试层

| 层 | 内容 | 硬件 |
|---|---|---|
| Core unit/property | digest、effect、state、classification | 否 |
| Artifact unit/fuzz | tar/PAC、bounds、malicious input | 否 |
| Projection contract | public/private action/effect completeness | 否 |
| Provider contract | permit、plan、receipt、reconcile、recovery | transcript |
| Transport replay | USB/tool/hotplug/fault | transcript |
| Authority contract | ArkDeck adapter、permit idempotence | 否 |
| Crash campaign | 双 journal/fsync window | 否 |
| OS integration | IPC/ACL/process/sandbox/driver mock | 部分 |
| Physical acceptance | exact device + real fault matrix | 是 |
| ArkDeck Golden Journey | App/Runtime/ArkForge/真机 | 是 |

### 19.2 必测安全场景

- wrong device；
- 同型号替换；
- duplicate candidate；
- identity strength drift；
- artifact/hash drift；
- profile/provider/toolchain drift；
- public/private projection mismatch；
- permit expired；
- permit duplicate；
- permit receipt 丢失；
- 每个 write 前后断连；
- exit 0 without semantic success；
- negative marker after possible effect；
- tool stall；
- daemon crash；
- ArkDeck Runtime crash；
- host power loss；
- cancel during critical step；
- outcomeUnknown repeated start；
- uncertain effect unbounded；
- incomplete recovery coverage；
- superseding recovery itself unknown；
- 读窗外 readback 判定(uniform 0xCC 不得冤判为假写)；
- 重枚举瞬态 malformed 观测与模式别名；
- 16 epoch/四小时预算耗尽。

### 19.3 DAYU200 真机

- HDC normal 和 Loader 起始模式；
- 单设备、多设备、其他 RockUSB 干扰；
- 至少两个合法 firmware build；
- 九分区；
- read-domain-aware declared verification(含 typed-skip receipts)；
- reset/re-adopt/build；
- legacy/new parity；
- complete-overwrite recovery；
- crash/cancel/fault campaign；
- packaging/signing/tool identity。

### 19.4 DAYU600 真机

证据门前只运行：

- PAC inspect；
- descriptor/probe；
- capture；
- transcript replay；
- wrong identity；
- PlanAssessment；
- start unavailable。

证据门通过后才增加：

- FDL；
- erase/write/verify；
- disconnect；
- recovery；
- complete Golden Journey。

---

## 20. Threat Model 与 Failure Matrix

### 20.1 主要威胁

| 威胁 | 控制 |
|---|---|
| Agent 提交错误目标/固件 | semantic Operation、exact binding、immutable plan |
| 恶意 artifact | sandbox、limits、fuzz、CAS |
| 设备替换 | unique lineage、fresh facts、same session recheck |
| Provider 错误地址 | Profile allowlist、public/private projection、digest |
| vendor tool 替换 | fixed hash/signature/ACL/no PATH |
| rogue local process | inherited controller handle、peer identity、no public execute |
| USB spoof | protocol identity、lineage、postflight |
| crash/power loss | dual journal、permit、unknown no replay |
| downgrade | version/digest pin、no reinterpretation |
| recovery misuse | distinct plan/new authority/coverage proof |

### 20.2 Failure Matrix

| 场景 | disposition | 新 dispatch |
|---|---|---|
| artifact mismatch | unavailable | 0 |
| profile unknown | unavailable | 0 |
| wrong device | unavailable/confirmed failed before effect | 0 |
| permit missing/expired | paused/unavailable | 0 |
| facts changed after permit | unavailable/reconciliation required | 0 |
| protocol no-effect NACK | confirmed failed | 0 |
| write may have started, no receipt | outcomeUnknown | 0 |
| exit 0, marker absent | outcomeUnknown | 0 |
| receipt durable, checkpoint missing | recover checkpoint | 不重执行 |
| read-only reconcile proves success | original action reconciled | 0 |
| read-only reconcile inconclusive | outcomeUnknown | 0 |
| recovery coverage incomplete | recovery ineligible | 0 |
| recovery coverage complete + ArkDeck admission | 新 recovery epoch | exact new plan only |
| recovery itself unknown | new possible effects join union | 重新证明后决定 |
| budget exhausted | blocker | 0 |

---

## 21. 渐进迁移

### 21.1 原则

- 不转换 active legacy job；
- 不重解释 legacy plan digest；
- legacy unresolved intent 只读 decode；
- shadow 模式不双写；
- backend 只能在 external intent 前选择；
- ArkForge unknown 不能自动切 legacy；
- migration cutover 必须真实 DAYU200 pass；
- 新 Provider/Profile/Operation 与生产代码、测试、真机证据同车。

### 21.2 阶段

#### Stage A：ArkForge 独立 read-only vertical

- workspace；
- neutral Core/Authority API；
- artifact import；
- DAYU200 inspect/profile；
- discover/probe；
- executable plan materialization but start disabled；
- public/private projection；
- current plan/effect parity。

该阶段在 ArkForge 工程交付，不产生 ArkDeck docs-only PR。

#### Stage B：DAYU200 production cutover

- daemon/IPC；
- ArkDeck authority adapter；
- ManagedDeviceControlPort；
- durable engine；
- fixed-tool Rockchip Provider；
- complete-overwrite recovery；
- generic ArkDeck integration；
- real DAYU200 parity/fault/recovery；
- packaging/signing/license：arkforged 与捆绑工具的签名、entitlement、LaunchAgent 与打包契约对齐 #1299 后的 helper signing 体系与当前运行时校验器语义(entitlement 死锁教训见 2.1/AD-007)——显式设计工作项，不是打包杂务；
- flash.dayu200 compatibility alias。

ArkDeck 侧作为一个垂直产品 PR。

#### Stage C：DAYU600 research/plan-only

- PAC research parser；
- evidence ledger；
- USB/FDL capture；
- DAYU600 Profile；
- Unisoc PlanAssessment；
- ArkDeck 同一 UI/API 展示；
- production start unavailable。

#### Stage D：DAYU600 production

- evidence gate 全通过；
- native/fixed approved transport；
- executable plan；
- semantic receipt；
- recovery；
- real DAYU600 Golden Journey；
- Provider/Profile maturity 发布。

### 21.3 Legacy 删除门

只有满足以下条件才删除 Swift Rockchip lowering：

- DAYU200 production cutover；
- agreed consecutive real passes；
- fault/recovery campaign；
- no unexplained parity drift；
- legacy history decoder 存在；
- current in-flight jobs terminal；
- rollback package verified；
- architecture guard 通过。

---

## 22. 垂直实施任务

### AF-V1：ArkForge Core + DAYU200 read-only parity

**目标**

建立独立 ArkForge 工程，并让 DAYU200 完成：

~~~text
artifact import
→ inspect
→ profile validation
→ discover/probe
→ public/private plan materialization
→ plan/effect parity
~~~

**生产代码**

- Rust workspace；
- neutral Authority API；
- Artifact/CAS；
- DAYU200 parser/profile；
- Rockchip read-only probe；
- PlanAssessment/FlashPlan；
- deterministic digest；
- projection validator；
- daemon read-only API；
- golden transcript 库(GJ-4 campaign receipts ECAMP-96EFFF15 / ECAMP-31E041BC 为种子)。

**验收**

- Core 不依赖 ArkDeck/vendor；
- current DAYU200 archive facts parity；
- unknown member/partition fail closed；
- private action digest 覆盖；
- startExecution disabled；
- unit/fuzz/transcript tests；
- Profile 含 readDomain 与 per-target 验证强度，与 AD-006 一致；
- DAYU200 整包 CAS 导入在声明预算内(10.2)；
- 无设备 mutation。

### AF-V2：DAYU200 ArkForge production cutover

**目标**

在 ArkDeck 使用 ArkForge 完成真实 DAYU200：

~~~text
inspect
→ probe
→ plan
→ authorize
→ execute
→ verify
→ reconcile
→ complete-overwrite recovery when eligible
~~~

**生产代码**

- ArkForge durable engine；
- ArkDeck adapter；
- StepPermit；
- ManagedDeviceControlPort；
- Rockchip fixed-tool Provider；
- generic Runtime integration；
- generic UI；
- compatibility alias；
- legacy decoder；
- arkforged signing/entitlement/packaging 契约(对齐 #1299 体系与校验器语义)。

**验收**

- real DAYU200 full flash pass；
- exact identity/multi-device；
- nine partitions/userdata；
- read-domain-aware verification(readback/typed-skip)+ build postflight；
- rebind 瞬态容忍与 normal 别名真机复验；
- crash/cancel/fault；
- outcomeUnknown no replay；
- eligible complete-overwrite recovery；
- ArkDeck production lowering 无 Rockchip command/address。

### AF-V3：DAYU600 evidence + plan-only

**目标**

让 ArkForge/ArkDeck 使用相同 API 对 DAYU600 完成可信研究：

~~~text
PAC inspect
→ USB discover/probe
→ profile candidate
→ PlanAssessment
→ evidence requirements
→ start unavailable
~~~

**验收**

- bluetool static evidence 纳入 ledger；
- PAC parser ResearchOnly；
- exact unknown list；
- descriptor/transcript capture；
- wrong device tests；
- parser fuzz；
- startExecution 无 bypass；
- 未把 plan-only 记为真机刷写通过。

### AF-V4：DAYU600 production execute

**前置条件**

第 17.5 节所有证据门 PASS。

**目标**

实现 Unisoc Provider/Transport，并完成 DAYU600 真实刷机、验证和恢复。

**验收**

- ArkDeck 代码不增加 DAYU600/Unisoc/PAC/FDL 分支；
- exact target；
- PAC/FDL/partition/effect 完整；
- typed protocol receipt；
- fault/cancel/outcomeUnknown；
- recovery coverage；
- real DAYU600 Golden Journey；
- platform support 仅按实测声明。

---

## 23. ADR-0001：本地 daemon + Protobuf IPC

**状态：Proposed**

### 23.1 方案比较

| 维度 | Rust C ABI | 一次性 CLI | 本地 daemon |
|---|---:|---:|---:|
| Swift 接入 | 中 | 高 | 高 |
| 崩溃隔离 | 低 | 中 | 高 |
| durable job | 低 | 低 | 高 |
| 重连 | 低 | 低 | 高 |
| streaming | 中 | 中 | 高 |
| authority handshake | 低 | 低 | 高 |
| USB hotplug/session | 中 | 低 | 高 |
| 跨平台 | 中 | 高 | 高 |
| packaging | 中 | 高 | 中 |

### 23.2 推荐理由

- parser/USB/tool 崩溃与 Swift 隔离；
- durable job；
- reconnect/watch；
- bidirectional StepPermit；
- centralized USB/tool permission；
- public read-only 与 controller execute 分离；
- Provider 版本和 plan store 可 pin。

### 23.3 不选择 C ABI

- native crash 拖垮 ArkDeck；
- async ownership/panic/allocator/versioning 复杂；
- 无法自然承载 durable job；
- 最终仍会演化成服务层。

### 23.4 不选择一次性 CLI

- 无法稳定保持跨模式 identity/session；
- permit/receipt 双向流困难；
- process exit 容易被误判；
- crash 后难以恢复同一 job；
- outcomeUnknown 和禁止 replay 难闭合。

CLI 仅用于 read-only/offline diagnostics。

---

## 24. Evidence Ledger

| ID | 事实 | 来源 | 等级 | 状态 |
|---|---|---|---|---|
| AD-001 | ArkDeck authority/provider/recovery contract | [provider-contracts.md](../../ArkDeck/openspec/contracts/provider-contracts.md) | C | confirmed |
| AD-002 | Flash complete-overwrite supersession | [flashing spec](../../ArkDeck/openspec/specs/flashing/spec.md) | C | confirmed |
| AD-003 | DAYU200 published operation/recovery contract | [flash.dayu200.json](../../ArkDeck/Catalog/operations/flash.dayu200.json) | C | confirmed |
| AD-004 | BlueTool contains separate DAYU600/UNISOC/PAC path | [bluetool-analysis.md](../../ArkDeck/openspec/changes/chg-2026-026-macos-rockchip-flash-ui/bluetool-analysis.md) | C/D | confirmed static evidence |
| AD-005 | Current Rockchip Provider/tool/profile implementation | ArkDeckKit production sources | C | confirmed |
| AD-006 | DAYU200 板端 RockUSB 读写面不对称：`rl` 读面自扇区 65536(32 MiB)起结构性盲区、窗口外恒 uniform 0xCC；擦除介质亦读为 0xCC；`wlx` 写面全盘可达；读窗大小须每次执行实测 | GJ-4 真机 campaign ECAMP-96EFFF15 / ECAMP-31E041BC；PR #1066–#1070；[RockchipRuntimeActionHost.swift](../../ArkDeck/Packages/ArkDeckKit/Sources/ArkDeckWorkflows/DeviceProviders/RockchipRuntimeActionHost.swift) `characterizeMediumReadDomain` 与 [RockchipRuntimeCompositionContractTests.swift](../../ArkDeck/Packages/ArkDeckKit/Tests/ArkDeckContractTests/RockchipRuntimeCompositionContractTests.swift) | C | confirmed(真机定案) |
| AD-007 | macOS Rockchip 组件 entitlement 死锁：运行时校验器要求 app-sandbox+inherit 与打包契约(#1052)要求空 entitlements 互斥，以修改校验器收口；spec/ADR 对齐留白 | 2026-08-04 定案；#1299 helper signing 现代化；AFD-0003 | C + A | **resolved**(2026-08-16，AFD-0003)；证据账本为正本 |
| FB-001 | Fastboot host-driven protocol and semantic status | [Android official fastboot README](https://android.googlesource.com/platform/system/core/+/master/fastboot/README.md) | A | confirmed; pin revision |
| FW-001 | fwupd daemon/plugin lifecycle | [fwupd official source](https://github.com/fwupd/fwupd) | A/B | confirmed; pin revision |
| DFU-001 | DFU detach/download/upload/reset | [dfu-util official manual](https://dfu-util.sourceforge.net/dfu-util.1.html) | A | confirmed |
| UUU-001 | UUU multi-stage protocol/tool model | [NXP mfgtools official source](https://github.com/nxp-imx/mfgtools) | A/B | confirmed; pin revision |
| RK-001 | rkdeveloptool source/tool behavior/license | [Rockchip official repository](https://github.com/rockchip-linux/rkdeveloptool) + pinned local source | A/B | pin required |
| UNI-U01 | UIS7885 PAC/FDL wire protocol | official docs/capture | U | missing |
| USB-001 | libusb transport substrate | [libusb official source](https://github.com/libusb/libusb) | A | confirmed; pin revision |
| IPC-001 | Protobuf evolution rules | [Protobuf official guide](https://protobuf.dev/programming-guides/proto3/) | A | confirmed |
| DIG-001 | deterministic CBOR | [RFC 8949 §4.2](https://datatracker.ietf.org/doc/html/rfc8949#section-4.2) | A | confirmed |

### 24.1 Ledger 规则

- URL 必须补固定 revision；
- ArkDeck 仓内证据以 `../../ArkDeck/` 相对路径引用(ArkForge 与 ArkDeck 同级目录布局)，内容以本文审计基线 commit 为准；
- evidence bytes/binary/artifact/capture 记录 SHA-256；
- D/U 不能独立支持 execute；
- ProductionVerified 必须引用 evidence set；
- evidence 状态变化版本化，不改写历史；
- 许可证未知默认不可再分发；
- simulation/plan-only 不记 real hardware pass。

---

## 25. 架构验收标准

ArkForge 架构通过必须满足：

1. ArkDeck 对 DAYU200/DAYU600 使用相同 client/adapter；
2. Agent/App 不提交 planID；
3. ArkDeck production 不含厂商命令、USB identity、PAC/FDL 和分区地址逻辑；
4. ArkForge Core 不依赖 ArkDeck；
5. ArkDeck Runtime 保留唯一 authority；
6. public/private plan 完整绑定；
7. 所有 destructive effects 执行前 materialize；
8. private action 无 public projection 时拒绝；
9. 每个 mutation/destructive action 需要 exact StepPermit；
10. fresh device facts 在 dispatch 前再次验证；
11. HDC server 仍由 ArkDeck 管理；
12. exit 0 不等于 success；
13. outcomeUnknown 永不 replay；
14. read-only reconcile 不产生 mutation；
15. complete-overwrite recovery 是 distinct plan/epoch；
16. original unknown outcome 不改写；
17. DAYU600 证据不足时没有 executable planID；
18. maturity 按精确组合发布；
19. 新设备不会降低错误设备保护；
20. DAYU200 真实迁移不回退；
21. DAYU600 只按真实验收声明支持；
22. 新 Operation/Provider/Profile 已经维护者 review；
23. verification 三态落地：TypedSkip 不计为 verified，读域外不声明 readback 强度；
24. rebind 瞬态容忍与模式别名为 Profile/Expectation 显式声明。

---

## 26. 最终建议

ArkForge 值得独立建设，但独立的核心不是“把命令包装成统一 CLI”，而是建立稳定的设备无关刷机领域模型：

~~~text
Artifact facts
+ Device/Profile facts
+ Provider capability
+ immutable public plan
+ immutable private plan digest
+ exact EffectSet
+ external Authority permit
+ semantic receipt
+ durable outcome/recovery
~~~

首个生产目标应保持单一：

> 让 DAYU200 通过 ArkForge 完成与当前路径等价或更强的真实刷机、验证、未知结果处理和完整覆盖恢复，同时让 ArkDeck production lowering 不再理解 Rockchip。

DAYU600 同期只进入严格 evidence lane。直到 PAC、FDL、USB identity、协议、数据影响、恢复和许可全部得到可复现证据，ArkForge 只能返回 PlanAssessment 和 UNAVAILABLE。

当 DAYU200 与 DAYU600 最终都通过时，ArkDeck 看到的是同一套语义 API、安全计划和结果；设备差异、协议差异和工具差异全部由 ArkForge 内部 Profile、Parser、Provider 与 Transport 吸收。

---

## 27. 当前实现状态附录（2026-08-19，TASK-NRU-004）

第 0.1 节第 8 项、第 16.1 节的 vendor-first 路线及第 18.2 节的旧 backend
示例仅记录最初提案的实施顺序，不再描述当前实现。当前发布实现只存在
`arkforged` 原生 RockUSB transport；vendor 二进制、端口选择、CLI 参数、命令
lowering/parser 与迁移 fallback 均已删除。现行 backend 事实以
`profiles/dayu200.yaml` 为准。
