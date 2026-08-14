# DAYU200 只读真机取证 — 2026-08-14

> 类别：真机观测(read-only)。**destructive dispatch = 0**：全程未写入任何分区，
> 未调用 `wl`/`wlx`，未导入或引用任何固件归档。执行的设备命令只有
> `hdc target boot loader`(模式转换)、`rkdeveloptool ld/ppt/rl`(读)、
> `rkdeveloptool rd`(复位)。
>
> 板子起止状态一致：HDC-normal → Loader → HDC-normal，同一端口、同一序列号、
> 同一 build。
>
> 工具：`arkforge-capture`(本仓)、ArkDeck 捆绑 rkdeveloptool、DevEco hdc。

## 1. 起始状态(HDC-normal)

```text
USB          0x2207:0x5000  loc=0x01200000  Rockchip / "HDC Device"
serial       150100424a544434520325834a7c4900
hdc target   150100424a544434520325834a7c4900  USB  Connected
```

经 typed control port 读取(`ManagedDeviceControlAction`)：

| action | 值 | evidence digest |
|---|---|---|
| `read-product-facts` (`const.product.model`) | `ohos` | `c99d8abf63fcf00f…4526e58d` |
| `read-build-facts` (`const.ohos.fullname`) | `OpenHarmony-7.0.0.37` | `aa5cac3279846d7f…957942ac` |

其他只读参数：`ohos.boot.hardware = rk3568`、`const.product.name = OpenHarmony 3.2`。

### 1.1 build version 是 per-artifact 事实，不是板级常量

GJ-4 campaign(2026-08-05)记录 `7.0.0.36`；本次实测 `7.0.0.37`。板子此后被重刷过。

这不是矛盾，而是对一处设计选择的验证：`profiles/dayu200.yaml` **不含**任何 build
版本字段，build facts 由 parser 从被哈希的 `system.img` **内部**提取
(`arkforge-artifact::dayu200::BUILD_FACT_KEYS`)。若当初把 `7.0.0.36` 钉进 profile，
今天这份 profile 就是错的。

ArkDeck 的实现注释独立地记录了同一条教训
(`DeviceProviderAdapters.swift`, `rebind-and-verify-build`)：
「归档名字说 7.0.0.35、build log 说 7.0.0.35，而它产出的设备答 7.0.0.36」。

## 2. 模式转换

```text
action  enterUpdater
argv    ["-t", "150100424a...4900", "target", "boot", "loader"]
accepted true
```

argv 由 typed action 决定，调用方无法提供(architecture.md 9.2)；取值与 ArkDeck 的
`RockchipHDCIntegrationProfile.enterLoaderArguments` 一致。

## 3. Loader 模式实测

```text
USB          0x2207:0x350a  loc=0x01120000  Rockchip / "USB download gadget"
rkdeveloptool ld:
  DevNo=1	Vid=0x2207,Pid=0x350a,LocationID=102	Loader
```

### 3.1 未测量的身份不被采纳(fail-closed 实证)

进入 Loader 后，`arkforge-capture observe` 报告：

```text
host sees 3 USB device(s)
  0x2207:0x350a loc=0x01120000 Rockchip  USB download gadget
profile org.openharmony.dayu200 recognizes 0 of them
```

当时 profile 只测量过 `0x2207:0x5000`。一个从未被测量的 VID/PID **不会**被当作
某个模式——这正是 11.2 要的行为。测量值随后写入 profile 的 `usbIdentities`(AD-008)。

### 3.2 身份字段跨模式变化(设计更正)

| 字段 | HDC-normal | Loader | 结论 |
|---|---|---|---|
| VID:PID | `0x2207:0x5000` | `0x2207:0x350a` | 变(模式指示量) |
| locationID | `0x01200000` | `0x01120000` | **变** — loader 挂在 `0x01100000` hub 之后 |
| 链路 | USB3(5 Gbps) | USB2 | 变 |

`locationID` 跨转换会变。本仓 transport 测试原先用 `TopologyPolicy::MustMatch`
构造 enter-loader 期望——真机证明那会**拒绝一块健康的 DAYU200**。

修正：serial 与 topology 的跨转换策略上收为 **Profile 声明的事实**
(`modeTransitions[].serialPolicy` / `.topologyPolicy`)，DAYU200 两条转换均记为
`may-change`，与 11.3「模式别名等价关系是 Profile 声明的事实」同一逻辑。

## 4. 设备自身分区表(`ppt`)与本仓解码逐值比对

设备答(LBA 为十六进制)：

| NO | LBA(hex) | 十进制 | 名称 | 本仓解码 | 一致 |
|---:|---|---:|---|---|:--:|
| 00 | 00002000 | 8192 | uboot | 8192 | ✅ |
| 01 | 00004000 | 16384 | misc | 16384 | ✅ |
| 02 | 00006000 | 24576 | bootctrl | 24576 | ✅ |
| 03 | 00007000 | 28672 | resource | 28672 | ✅ |
| 04 | 0000A000 | 40960 | boot_linux | 40960 | ✅ |
| 05 | 0003A000 | 237568 | ramdisk | 237568 | ✅ |
| 06 | 0003C000 | 245760 | system | 245760 | ✅ |
| 07 | 0043C000 | 4440064 | vendor | 4440064 | ✅ |
| 08 | 0063C000 | 6537216 | sys-prod | 6537216 | ✅ |
| 09 | 00655000 | 6639616 | chip-prod | 6639616 | ✅ |
| 10 | 0066E000 | 6742016 | updater | 6742016 | ✅ |
| 11 | 0067E000 | 6807552 | eng_system | 6807552 | ✅ |
| 12 | 00686000 | 6840320 | eng_chipset | 6840320 | ✅ |
| 13 | 0069E000 | 6938624 | chip_ckm | 6938624 | ✅ |
| 14 | 01308000 | 19955712 | userdata | 19955712 | ✅ |

**15/15 逐值一致**，顺序亦一致。

这补上了 AF-V1 验收里此前明写的缺口：`docs/evidence/AF-V1-acceptance.md` 2.2 节
原记「与厂商归档的字节级比对不在范围」。三方一致(Profile allowlist / 设备分区表 /
归档 manifest)中的**设备一侧现已由真机验证**；仍未验证的只剩厂商归档字节本身。

profile 的 9 个可写目标与 6 个 protected 目标，与设备表的划分完全对应：
可写的 9 个都在表中，protected 的 6 个(misc、bootctrl、sys-prod、chip-prod、
eng_system、eng_chipset)也都在表中且无归档成员支撑。

## 5. AD-006 读窗实测复现

`rl <sector> 1`，每次读一个扇区：

| 扇区 | 返回 | 判读 |
|---:|---|---|
| 1 | `EFI PART…`(varied) | 主 GPT，可读 |
| 8192 | varied, sha256 `ced869c8…` | uboot 真实内容，可读 |
| 32768 | uniform `0x00` | 窗口内 |
| 65535 | uniform `0x00` | **窗口内最后一个观测点** |
| 65536 | uniform `0xCC` | **边界：自此起 0xCC** |
| 65537 | uniform `0xCC` | 窗口外 |
| 131072 | uniform `0xCC` | 窗口外 |
| 245760 | uniform `0xCC` | 窗口外 — **system 分区** |
| 4440064 | uniform `0xCC` | 窗口外 — **vendor 分区** |

边界落在扇区 **65536**，与 AD-006 记录的 2026-08-04 观测(自扇区 65536 / 32 MiB 起
结构性盲区)一致。这是十天后在同一块板上的独立复现。

### 5.1 这次复现比原始观测更强

读取时该板**正在运行 OpenHarmony-7.0.0.37**——即 `system`(245760)与
`vendor`(4440064)分区里确实存放着一套可启动的系统。而 `rl` 对这两处返回
uniform `0xCC`。

因此：**窗口外的 uniform 0xCC 被现场证明不等于「未写入」**。这正是 2026-08-04
把九个分区冤判为「假写」的那一类错误，本次是它的活体演示。

`arkforge-core::verification` 对此的落点：读域不覆盖 → `TypedSkip`(不计任何
verified 强度、也不判失败)；读域覆盖且 uniform filler → `Failed{ErasedMediumFiller}`
单列判定。本次数据支持该三态划分。

`profiles/dayu200.yaml` 仍**不**钉死 65536——窗口大小是每次执行的实测事实，不是
板级常量；两次观测一致不足以把它变成常量。

## 6. 工具身份漂移(需要处理)

本机存在三个不同的 rkdeveloptool：

| 来源 | SHA-256 | 可用性 |
|---|---|---|
| ArkDeck 声明的 pin(`RockchipFlashProfile.pinnedToolchainFingerprint`) | `038a8a0e…c23611` | — |
| `/Applications/ArkDeck.app/Contents/MacOS/rkdeveloptool` | `231a05ef…a11c79e` | **可用**，本次全部读操作由它完成 |
| `/opt/homebrew/bin/rkdeveloptool` | `bbd7bdc0…6c9923` | **不可用**：`ld` 与 `-v` 均挂起(无设备操作也挂起) |

两点：

1. 捆绑件与声明 pin 不一致。最可能的解释是打包后重签名改变了二进制哈希
   (仓内存在 `/private/tmp/ArkDeck.app.before-rk-signfix-2849c5c1-*`)，若如此，
   pin 应明确其指向签名前还是签名后的产物——否则运行时校验无从执行。
   这与 AD-007(entitlement 死锁)是同一片区域。
2. homebrew 版本在本机不可用且哈希不同。ArkForge 的 maturity 组合键把 toolchain
   backend digest 计入，因此换一个 rkdeveloptool 就是**换一个组合**，不继承任何
   ProductionVerified —— 本次实测正是该设计要防的情形。

## 7. 未做

- 任何写入(AF-V2)；
- 厂商 `images.tar.gz` 的字节级比对(归档不在仓内)；
- Maskrom 模式(未进入)；
- 读窗边界的二分收敛(只取了 9 个探测点，足以定位到 65535/65536 之间)。
