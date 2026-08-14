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

## 6. 工具身份 — AD-010 澄清

初次记录时我把此处写成「三个哈希都不等于 pin」。**那个说法是错的**：ArkDeck 有
**两个**有意不同的 pin，我只对了其中一个。完整结论如下。

### 6.0 ArkDeck 的两个 pin 是设计，不是失误

`RockchipDeviceDiscovery.swift:9–28` 声明两个 profile，**同一 upstream commit
`304f073752fd25c854e1bcf05d8e7f925b1f4e14`**，两个不同的本地构建，两种 access policy：

| profile | executableSHA256 | 用途 | accessPolicy |
|---|---|---|---|
| `pinnedReadOnlyDiscovery` | `bbd7bdc0…9923` | 「clean, non-quarantined build approved **only** for E0/read-only `ld` discovery」 | `.userSelectedSecurityScopedBookmark` |
| `pinnedProduction` | `038a8a0e…3611` | 「compatibility identity consumed by the existing **destructive** Flash authorization, execution, and manifest surfaces」 | `.installedOrdinaryBookmark` |

同一份源码的两个构建,按用途分权——只读发现与破坏性刷写用不同的二进制、不同的
bookmark 策略。这正是 ArkForge maturity 组合键把 toolchain backend digest 计入的理由:
**它们是两个组合**,不互相继承任何 ProductionVerified。

### 6.1 本机四个二进制的归属

| 路径 | 签名后 SHA-256 | 剥签名后 | 归属 |
|---|---|---|---|
| `~/dayu200-rehearsal/rkdeveloptool/rkdeveloptool` | `038a8a0e…c23611` | `2081fb90…` | **= `pinnedProduction`,逐字节命中** |
| `/opt/homebrew/bin/rkdeveloptool` | `bbd7bdc0…6c9923` | `016d468f…` | **= `pinnedReadOnlyDiscovery`,逐字节命中** |
| `/Applications/ArkDeck.app/Contents/MacOS/rkdeveloptool` | `231a05ef…a11c79e` | `c31e8a3f…` | 两个 pin 均不符 |
| `/private/tmp/ArkDeck.app.before-rk-signfix-…/…/rkdeveloptool` | `1e54a0cd…256739` | `c31e8a3f…` | 同上,签名前副本 |

**AD-010 结论:pin 不存在「指向签名前还是签名后」的歧义。** `038a8a0e…` 指向一个
确定的本地构建,该文件此刻就在本机、逐字节命中。ArkDeck 的
`authorizations/README.md` 亦记「`~/dayu200-rehearsal/rkdeveloptool/rkdeveloptool`
实测命中」。

签名确实改变哈希——捆绑件与其签名前副本剥签名后同为 `c31e8a3f…`,签名后分别是
`231a05ef…` 与 `1e54a0cd…`。但这与 pin 无关:捆绑件剥签名后是 `c31e8a3f…`,而
rehearsal 构建剥签名后是 `2081fb90…`,**两者本就是不同构建**,不是同一构建的签名差异。

### 6.2 另外三条,逐条追到底后全部收口

写下它们时我以为都是 ArkDeck 侧待处理项。逐条查 ArkDeck 源码与 openspec 后,**没有一条
构成可提交的缺陷**。记录在此,连同判定依据。

**AD-013(原判「最要紧」)— 已知,且早有夹具。** `rkdeveloptool ld` 对处于 HDC-normal
的板子报 `Mode=Maskrom`:

```text
本次实测   DevNo=1	Vid=0x2207,Pid=0x5000,LocationID=102	Maskrom
ArkDeck 夹具 DevNo=1	Vid=0x2207,Pid=0x5000,LocationID=2	Maskrom
```

`Tests/ArkDeckContractTests/Fixtures/Rockchip/Discovery/1.0.0/maskrom.stdout.bin` 与本次
实测**同形**,registry 记 `{"mode":"Maskrom","reason":"providerDoesNotSupportMaskrom"}`,
契约测试 `testTEST_AC_FLASH_001_01_…BlockWithoutGuessing` 覆盖。

更要紧的是 ArkDeck 的判定顺序本就正确
(`RockchipDeviceObservation.providerPreflightDisposition`):

```swift
guard usbVendorID == 0x2207, usbProductID == 0x350a else { return .blocked(.deviceNotExpectedRockUSB) }
guard mode == .loader else { return .blocked(.maskromNotSupported) }
```

**先判 VID/PID,再判 mode。** 一台 `0x5000` 的板子在 mode 字段被读到之前就已被拦下。
ArkDeck 与 ArkForge 用的是同一道防线,各自独立到达。

**AD-012 — 非缺陷。** 捆绑组件走的是另一条声明线:CHG-2026-036 的
`package-receipt.json` 逐包声明
`component.signedSHA256` / `component.unsignedSHA256` **成对**身份。也就是说,「签名前
还是签名后」这个问题 ArkDeck 早就分开记了。本机安装的是比归档收据更新的一个包,所以
两个哈希都对不上归档值——这是版本差异,不是声明缺失。

**AD-011 — 原因已查明,是 quarantine,不是构建缺陷。**

先说清一件我一开始搞反的事:`bbd7bdc0…` 虽然哈希等于 `pinnedReadOnlyDiscovery`,但那个
pin 是 E0 表征期从 homebrew 二进制登记的,**ArkDeck 自己并不用它**——ArkDeck 编自己的
(见 6.4)。而且 ArkDeck 早在 **2026-07-24** 就因**源码溯源漂移**把这条路径挡死了:探针从
可执行文件的父 checkout 取 upstream 收据,`/opt/homebrew` 的 HEAD 是 `7c2bb3b2…` 而非注册
的 `304f0737…`,记录原文「binary hash equality does not authorize silently replacing or
ignoring the source-provenance check」。

本次另外查清了它**为什么挂**:

| 步骤 | 观察 |
|---|---|
| `sample` 抓栈 | 2 秒 1654 个采样**全部**停在 `_dyld_start + 0`——从未进入 `main`,故 `-v` 亦挂 |
| `DYLD_PRINT_LIBRARIES=1` | 一行未输出,连第一个 dylib 都没加载 |
| `xattr` | `/opt/homebrew/bin/rkdeveloptool` 带 `com.apple.quarantine`;两个依赖 dylib 都不带 |
| 对照实验 | 复制一份、`xattr -c`、**哈希不变**(`bbd7bdc0…`)→ 立即正常返回 `DevNo=1 …` |

结论:**quarantine 导致 Gatekeeper 评估在 dyld 阶段阻塞**。而 `pinnedReadOnlyDiscovery` 的
代码注释原文就是「The clean, **non-quarantined** build」——本机这份哈希仍对,状态已漂出
pin 的描述。ArkDeck 的 E0 collector 本就检查 quarantine(source-drift 记录写它「stopped
before codesign/quarantine checks」),只是那次更早地被源码漂移挡住了。

本机若要用它:`xattr -d com.apple.quarantine /opt/homebrew/bin/rkdeveloptool`。但更该做的是
用 ArkDeck 自己的构建——见下。

### 6.4 ArkDeck 自建构建:为什么这类问题在它那里不存在

`openspec/integrations/rockchip/bundled-component/1.0.0/recipe.json`
(`rockchip-component-build@1.0.0`)是一个**密闭、可复现**的构建:

- 上游钉死:`rockchip-linux/rkdeveloptool` commit `304f0737…`,archive `389ba41a…`,
  tree `9908d5bd…`,`upstreamSourceModifications: "none"`;
- **libusb 1.0.30 静态链入**,tarball 带 GPG 签名与指纹校验;
- 密闭:`homebrewBuildPaths: denied`、`callerPATH: ignored`、`networkAfterFetch: denied`、
  `SOURCE_DATE_EPOCH`/`ZERO_AR_DATE` 固定、`normalization: forbidden`;
- 双 builder(builder-a / builder-b)独立构建,verdict「byte-identical unsigned Mach-O」;
- 产物直接依赖被 `directDependencyAllowlist` 限定为七个系统库。

本机 `otool -L` 实测三者:

| 构建 | libusb | C++ 运行时 | 结果 |
|---|---|---|---|
| ArkDeck 自建 | **静态链入** | Apple `libc++` | 依赖恰为 recipe 允许的七项,可用 |
| homebrew | 动态 `/opt/homebrew/…/libusb-1.0.0.dylib` | **GCC `libstdc++.6`** | quarantine → 挂 |
| rehearsal(`pinnedProduction`) | 动态同上 | Apple `libc++` | 可用 |

静态链接 + 系统库白名单,正是让捆绑件既不依赖宿主 homebrew 状态、也不暴露在这类
dylib/quarantine 耦合下的原因。用户所说「homebrew 里的 rk 是错的,ArkDeck 是自己编译的」
在 recipe 与实测两侧都成立。

**结论:无 OpenSpec change 可提。** AD-012/AD-013 ArkDeck 已覆盖;AD-011 是本机 quarantine,
且 ArkDeck 早已因源码漂移挡住该路径并改用自建构建。

### 6.3 ArkDeck 已知的相邻缺陷

`chg-2026-025/tasks.md` 记录了同一片区域的一个已知缺陷:缺省
`RockchipDeviceDiscoveryAdapter()` 绑 `pinnedReadOnlyDiscovery`(bbd7),而
production composition 声明 `pinnedProduction`(038a),声明门比较这两个编译期常量
**恒不等**。修复方向已裁定为 A(在 composition 处显式注入正确 profile)。本次实测与
该记录一致,不构成新缺陷。

## 7. 未做

- 任何写入(AF-V2)；
- 厂商 `images.tar.gz` 的字节级比对(归档不在仓内)；
- Maskrom 模式(未进入)；
- 读窗边界的二分收敛(只取了 9 个探测点，足以定位到 65535/65536 之间)。
