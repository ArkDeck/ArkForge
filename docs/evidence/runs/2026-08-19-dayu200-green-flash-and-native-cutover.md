# 2026-08-18/19 DAYU200 全量刷写：首过与原生换轨

两次端到端 `succeeded` 的 `flash.dayu200`，中间隔着 NRU-001..004 的原生化：

- **Run A(2026-08-18)**：`job-a4b7d539571082b1958ebaaf2c14bd2c`——**首次真机全量
  刷写通过**(AF-V2 第一验收项)。执行面是当日的 **fixed-tool 运行时**(捆绑密闭构建
  `rkdeveloptool`)，authority 分界(ArkDeck 签发 permit、arkforged 执行)即今天的形状。
- **Run B(2026-08-19)**：`job-b00e006a1fbe9d6de388efab4138b9a2`——**vendor 运行时
  移除后的原生全量通过**。daemon 二进制内已无任何 vendor 字符串，写入走
  `NativeRockUsbPort` + `RockUsbProtocol`(NRU-004)。

两次运行的 readback 证据形状逐分区一致(见 §4)，postflight `summary` 除
stdout 摘要外逐键一致——原生面复现了 vendor 面的全部可观测语义。

本文由台架一手存储写成：ArkDeck job 记录
`~/Library/Application Support/ArkDeck/Agentd/jobs/<job>/`、arkforged journal
`…/Agentd/arkforge/store/jobs/`、委托步骤回执 `…/Agentd/rockchip-runtime/<job>/`。

---

## 1. 两次运行的身份

| 项 | Run A(首过) | Run B(原生) |
|---|---|---|
| ArkDeck job | `job-a4b7d539571082b1958ebaaf2c14bd2c` | `job-b00e006a1fbe9d6de388efab4138b9a2` |
| 结果 | `succeeded`,`outcomeUnknown: false` | `succeeded`,`outcomeUnknown: false` |
| arkforged job | `JOB-000001A013991062`(23 permit accepted→consumed) | `JOB-000001A0180894B7`(同 23) |
| 时间(UTC) | 06:39:00 → 06:48:49(9 分 49 秒) | 03:19:18 → 03:30:24(11 分 06 秒) |
| 计划摘要 | `c8837ff0137b037e06b96129fce71951337b58d051a2bc5c006dfff015eebc5c` | `dde51435593d77027d5c111d00711c95b69bd2331ea6137fae2c025efe30c4cb` |
| daemon 构建 | `aa7fe808…0085`(08-18 11:18 安装;`strings` 含 `rkdeveloptool` ×13——fixed-tool 运行时) | `f3dfc624…66d9`(08-19 10:29 安装,晚于 c049a11;`strings` 零 vendor 命中,含 `always the native implementation`) |
| 回执 `providerExecutableSHA256` | `231a05ef…c79e` = 捆绑 vendor 构建(AD-023 那份;当日 schema 以它为 provider 身份锚) | `f3dfc624f24c0e7ebd586b12acd0d64c145721f17e96db5557e07b2fbb1766d9` = daemon 自身(NRU-003 换源后) |
| 目标 | `TGT-958780b2ffb7`,binding revision 4,stable identity `94a25a89…6f42` | 同左 |
| 归档 | build `OpenHarmony-7.0.0.37`,SHA-256 `4fd35765fa75b9e2ce7c11f614144804f72efdc955a197e657014df1349ac674` | 同左 |

backend 归属是可复核的**二进制事实**，不靠回忆：Run A 的 daemon 安装于
08-18 11:18，而原生读路径的第一个 commit(`a935798`)16:20 才进 main——那台
daemon **不可能**含原生代码；Run B 的 daemon 构建晚于移除 vendor 的
`c049a11`(08-19 10:05)，且其回执自证摘要 `f3dfc624…`。

## 2. 中间发生了什么(NRU-001..004)

| 时刻(08-18/19 本地) | 事件 |
|---|---|
| 08-18 14:48 | Run A `succeeded`(fixed-tool) |
| 16:20 / 16:41 | `a935798`/`26aa527`：原生读路径 + 真机 Loader 读 parity(证据在仓：`crates/arkforged/tests/evidence/2026-08-18-task-nru-001-read-parity.txt`) |
| 17:05 | `3567484`：原生写路径 + 复位(NRU-002,台架双轨互证) |
| 20:26 / 21:58 | 其间两次全量 `succeeded`(`job-9df5352a…`、`job-f20afd04…`),伴随双轨与默认切换窗口(backend 未逐一归属,不据此声称) |
| 21:24 | `8129a7a`：默认切原生(NRU-004 前半) |
| 08-19 10:05 | `c049a11`：**移除 vendor RockUSB 运行时**,`--rkdeveloptool*` 旗标退场 |
| 11:30 | Run B `succeeded`(纯原生二进制) |

## 3. 写入面(两次相同的九个成员)

staging → 写前 revalidate → 写入 → 摘要比对(journal `transportEvidenceRecorded`
的 `imageSha256` + semantic receipt)：

| 分区 | 镜像字节 |
|---|---:|
| uboot | 4,194,304 |
| resource | 5,652,480 |
| boot_linux | 67,108,864 |
| ramdisk | 2,367,100 |
| system | 2,147,483,648 |
| vendor | 268,431,360 |
| updater | 20,713,404 |
| chip_ckm | 33,554,432 |
| userdata | 1,468,006,400 |

复位由 arkforged 自己的计划发出；ArkDeck 侧七个模式/善后步骤全部
`delegated <step> to the ArkForge lane's own plan`(enter-loader-mode、
wait-loader-disconnect、wait-loader-reconnect、rebind-loader-identity、
reboot-device、wait-for-hdc、rebind-and-verify-build)，两次运行相同。

## 4. readback 证据(windowed 读面,AD-006/AD-019 形状;两次逐分区一致)

| 判定 | 分区 |
|---|---|
| Verified | `uboot`(8192 起)、`resource`(28672 起) |
| Failed | `boot_linux`(40960 起) |
| TypedSkip | `ramdisk`(237568 起)以后六个 |

`boot_linux` 的 Failed 是**结构性的**，不是写入失败：它自扇区 40960 起、跨过
读窗边界(AD-006 实测 65536)，可读前缀加窗外恒 `0xCC` 的尾部不可能复现整镜像
摘要。写入完整性由写路径自己的摘要比对保证；readback 只贡献 verified 强度，
Failed/TypedSkip 均不计强度也不构成 job 失败。与 2026-08-15 彩排对照：
`resource` 从 Failed 变为 Verified(板上内容现在就是刚写入的归档内容)；
`boot_linux` 在三次记录中都受同一读窗边界限制。**原生读实现与 vendor 读面的
逐字节 parity 另有专项记录**(NRU-001 证据文件,见 §2)。

## 5. postflight(STEP-023 委托回执,两次 `summary` 语义一致)

~~~json
"summary": {
  "firmware": "OpenHarmony-7.0.0.37",
  "model": "ohos",
  "hdcIdentitySha256": "958780b2ffb7090d4f22cdc1f547f9804ed0f0b605e3020f384e5d4823dc7a7e",
  "usbTopology": "17956864",
  "verification": "exact-published-profile-and-bound-hdc"
}
~~~

期望构建值来自被写入归档(`verify-image-bundle` 声明 `OpenHarmony-7.0.0.37`)，
设备两次都答出同值；绑定身份经 bound-HDC 别名重认。各自发布
`post-flash-facts.json` 与 `post-flash-hilog.txt` 后 `finalizing → succeeded`。
两步回执的 subprocess 全部为 `hdc`(A:21,B:20)——委托回执里从无 vendor 命令痕迹。

## 6. 各自闭合什么

- **Run A 闭合 AF-V2 第一验收项**(`real DAYU200 full flash pass`)以及
  AF-V2-acceptance.md §3.3/§3.5 的真机半——用的是当日发布的 fixed-tool 组合。
- **Run B 闭合 NRU-004 的收尾问题**：vendor 运行时移除后,同一 lane、同一
  authority 分界、同一证据形状下全量刷写仍然全绿。toolchain 身份自此收敛为
  `arkforged` 自身构建摘要。
- maturity 是组合键：两次通过各发布**各自那一个**组合,谁都不自动成为
  `ProductionVerified`(AD-025,另需维护者决定)。

**仍未验证、本文不声称**：写入中途 SIGKILL 的真机 crash 语义、掉电(AD-017
open)、多设备 exact 绑定、complete-overwrite recovery 的 plan 物化
(Profile 仍 `supportsCompleteOverwrite: false`)。
