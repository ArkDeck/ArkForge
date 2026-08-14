# 2026-08-15 DAYU200 全量刷写彩排(read-only)

**设备写入次数：0。** 板子在本次全程未被写入任何分区；结束时已回到 `hdc-normal`，
`const.ohos.fullname` 仍为 `OpenHarmony-7.0.0.37`(刷写前后同值，因为没有刷写)。

本次做的是 AF-V2 的「除写入外的全部」：真实归档导入、真实计划物化、九个镜像真实落盘、
每一个动作降解为真实 argv、只读动作在真机上真实执行。九条 `wlx` 与一条 `rd` 被降解、
逐项前置校验、然后**不派发**。

---

## 1. 环境

| 项 | 值 |
|---|---|
| 主机 | macOS 25.6.0 (Darwin), arm64 |
| 板子 | DAYU200 / RK3568，序列 `150100424a544434520325834a7c4900` |
| 归档 | `version-Daily_Version-OpenHarmony_7.0.0.35-20260728_180253-dayu200_img.tar.gz` |
| 归档 SHA-256 | `6a023c738ac585b8a6f537c99f2ab2df95a5359fd6d4dd33150fad62e71f064e`(730,769,584 B) |
| rkdeveloptool | `/opt/homebrew/bin/rkdeveloptool`，SHA-256 `bbd7bdc0fb121d414fb61085e77211cc1fdd9a3b6c6b285c54380f70e56c9923` |
| hdc | DevEco SDK，SHA-256 `05b2bf7ad30201c082da336db28f8856952a2b2f49ac3404b96fdb4bf1a68f83` |
| 工具 | `arkforge-rehearse`(本次新增)、`arkforge-capture`(AF-V1) |

模式切换(`hdc target boot loader` 与 `rkdeveloptool rd`)由 `arkforge-capture`
在 `--i-am-changing-device-mode` 下显式发起，不是彩排工具自己做的。

---

## 2. 真实设备识别

两种人格都被 Profile 的实测 USB 身份认出，`identityStrength` 均为 `serialAndTopology`：

~~~text
hdc-normal      USB-2207-5000-01200000   0x2207:0x5000  "HDC Device"
rockusb-loader  USB-2207-350a-01120000   0x2207:0x350a  "USB download gadget"
~~~

注意 locationID 在模式切换中变了(`0x01200000` → `0x01120000`)，这正是 AD-008 之后
把 `topologyPolicy` 定为 `may-change` 的原因；本次是第二次独立复现。

---

## 3. 设备自己的分区表(AD-018)

`rkdeveloptool ppt` 的**真实**输出格式与我此前按文档写的解析器不同：

~~~text
**********Partition Info(GPT)**********\r\n
NO  LBA       Name                \r\n
00  00002000  uboot\r\n
...
14  01308000  userdata\r\n
~~~

三列、CRLF、十六进制**不带 `0x`**，且**没有 size 列**。我原来的解析器要求四列
带 `0x` 前缀的字段——它在我自己写的夹具上通过，在真机上一行也解析不出来。

已按实测重写，夹具换成上面这份逐字节的真实输出。派生规则写清楚了：
`size_sectors` 是**到下一个分区起点的距离**，是「不越界」的上界，不是设备声明的大小。
两者在本板上确实不同——归档给 `chip_ckm` 131072 扇区，而下一个分区在 13017088 扇区后
才开始，中间是未分配空间。

十五行与归档 `parameter.txt` 的十五行逐项一致，九个 Profile 目标全部在声明位置上，
六个 protected 分区全部被 Profile 认识 —— `check_conformance` 通过。

### 3.1 顺带修正的一处自造检查

原 `ValidatePartitionTable` 拿「Profile 九个目标的 CBOR 摘要」去比「设备十五行的文本摘要」。
这两个摘要**永远不可能相等**，我在真机上才发现。现已拆成两个各自对的检查：

- `planLayoutDigest` —— 计划所声明的布局摘要 vs 由 Profile 现场重算的同一摘要，
  抓的是「计划与它所引用的 Profile 是否已经漂移」；
- `check_conformance` —— 设备的表 vs Profile，抓的是三方一致(architecture.md 16.3)。

---

## 4. 读域实测(独立复现 AD-006)

~~~text
addressableMedium = windowed
readDomainDetail  = sector 1 read real data; sector 19955712 read uniform 0xCC
~~~

九个目标的 readback 三态判定，全部来自真机：

| 目标 | 起始扇区 | 判定 | 说明 |
|---|---:|---|---|
| uboot | 8192 | **Verified**(FullHash) | 在读窗内；板上 uboot 与归档 `uboot.img` **逐字节相同** |
| resource | 28672 | Failed(content-mismatch) | 在读窗内，读到真实内容，但板子跑的是 7.0.0.37 |
| boot_linux | 40960 | Failed(content-mismatch) | 同上 |
| ramdisk | 237568 | TypedSkip(skipped-lba-read-window) | 窗外 |
| system | 245760 | TypedSkip | 窗外 |
| vendor | 4440064 | TypedSkip | 窗外 |
| updater | 6742016 | TypedSkip | 窗外 |
| chip_ckm | 6938624 | TypedSkip | 窗外 |
| userdata | 19955712 | TypedSkip | 窗外 |

**读窗边界落在 40960 与 237568 之间**，与 AD-006 记录的扇区 65536 完全相容，
且这次是由一条与 AF-V1 capture 完全不同的代码路径独立测出来的。

那两个 `Failed` 是诚实的：什么都没写，板子上是 7.0.0.37 的内容，
拿 7.0.0.35 归档的 hash 去比当然不等。它们证明的是「读窗内的 readback 真的在比对内容」。

`uboot` 的 Verified 则是一个真实事实：两个 daily 之间 u-boot 没变。

### 4.1 顺带修正的一处顺序错误

原计划把 `readback-partition` 排在 `characterize-read-domain` **之前**。
architecture.md 16.2 写的是 `CharacterizeReadDomain + ReadbackPartition`——先测读面。
顺序反了的话，第一个 readback 会在没有任何读面测量的情况下去判定 uniform filler，
而这正是 AD-006 记录的那次冤案的成因。已改为先 characterize。

彩排里这个 bug 是以「ACT-013 REFUSED：读面尚未 characterize」的形式暴露出来的——
执行器拒绝了它，没有猜。

---

## 5. 九个镜像真实落盘

一次流式遍历，边解压边写边算 hash，逐个与 manifest 比对：

~~~text
staging   9 member(s)
          4,017,485,774 bytes in 44.12s (86.8 MiB/s)
~~~

九个镜像的 SHA-256 全部与 ArkDeck 的钉值一致(见 AF-V2.1 parity 测试)。
写入前每个都会重新读一遍全文件再验一次 hash(`StagedImage::revalidate`)——
彩排里九个全部 revalidate 通过。

---

## 6. 九条写入的降解与前置校验

每条都降解成了真实 argv，例如：

~~~text
wlx system /…/staging/system.img
   profile   allows system at sector 245760
   device    system @245760, 4194304 sectors to the next partition, image needs 4194304
   image     system.img revalidated (2147483648 bytes, 86357e57…)
~~~

九条全部通过三项前置校验(Profile 允许、设备表一致、镜像 revalidate)，
`system` 恰好铺满它的 4194304 扇区，其余八条都有余量。

**九条 `wlx` 与一条 `rd` 全部被扣留，一条也没有派发。**

### 6.1 彩排工具自己被抓到的一处缺陷

第一次运行时，`rd`(重启设备)走进了「执行只读动作」的分支，真的被派发了出去。
它没有生效，只是因为当时板子还在 `hdc-normal`，rkdeveloptool 根本连不上
(`creating comm object failed`)。这是运气，不是设计。

`RockUsbCommand::is_read_only()` 当时已经存在，我只是没用它。已改为：
凡非只读命令一律扣留。一个自称「只读」的工具派发了重启，这条得记下来。

---

## 7. 为什么停在写入之前

不是谨慎，是架构。

AF-V2 的目标原文是「在 ArkDeck 使用 ArkForge 完成真实 DAYU200」，
ArkForge 自己不持有 authority(architecture.md 3、8)。一次写入需要 StepPermit，
一张 permit 需要一个 authority 去签发，而本仓**刻意做不到**：
架构守卫禁止 `crates/arkforged` 引用签发函数(`the_daemon_never_mints_a_permit`)。

因此这里缺的不是代码，是一个决定：谁来做 authority。两条路都是真的：

1. **ArkDeck 做 authority**(architecture.md 22 AF-V2 原意) —— 需要 ArkDeck 侧的
   adapter/Runtime/UI 改动，走 OpenSpec + maintainer review；
2. **本仓新增一个 bench authority crate** —— 需要在 architecture.md 4.3 的
   crate 边界图里加一个成员，并在架构守卫的允许表里给它一行理由。

两条都要拍板，不是我能自行决定的。

另有一条与之独立的门：`RK-M02`。maturity 目前是 `hardwareGated`——
「AF-V2 要求先有一次真机全量刷写通过，这个组合才能是 ProductionVerified」。
这是 AF-V1 写下的门，它现在正按设计挡着。彩排产出的是 **PlanAssessment**，
不是 Executable plan；private plan 仍然带出来了，因为受阻的计划也必须能被审计
(architecture.md 6.3)。

---

## 8. 复现

~~~bash
# Loader 模式(会改设备状态)
arkforge-capture enter-loader --profile profiles/dayu200.yaml \
  --target <connect-key> --i-am-changing-device-mode

# 彩排(只读)
arkforge-rehearse \
  --archive ~/Downloads/version-Daily_Version-OpenHarmony_7.0.0.35-…-dayu200_img.tar.gz \
  --profile profiles/dayu200.yaml \
  --store <cas-dir> --staging <staging-dir>

# 回到 normal(会改设备状态)
arkforge-capture reboot-normal --profile profiles/dayu200.yaml \
  --i-am-changing-device-mode
~~~
