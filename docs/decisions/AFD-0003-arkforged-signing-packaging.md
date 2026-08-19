# AFD-0003：arkforged 的 macOS 签名 / entitlement / 打包契约

> 状态：Accepted(实施决定，非架构变更)
>
> 日期：2026-08-16
>
> 相关：architecture.md 2.1 / 15.1 / 16.1 / 21.2 Stage B / 24(AD-007)、
> AFD-0001、ArkDeck #1299(helper signing 现代化)、ArkDeck ADR-0003、
> ArkDeck `docs/release/rockchip-component-packaging.md`(TASK-BRC-003)

> **NRU-004 修订（2026-08-18）**：原生 RockUSB 已成为默认执行端口，发布包
> 只携带并签名 `arkforged`；不再复制、重签或在 receipt 中声明 vendor
> 二进制，也不再接受 `ARKFORGE_RKDEVELOPTOOL*` 打包输入。下文关于 vendor
> 签名、自检和 entitlement 的条款保留为迁移期显式 `--rockusb-port vendor`
> fallback 的运行时约束及历史实测，不再描述当前发布包形状。

## 背景

architecture.md 21.2 把这件事列为 Stage B 的**显式设计工作项，不是打包杂务**，
理由是它已经付过一次代价。

ArkDeck 的 macOS Rockchip 组件曾经卡在一个死锁上(AD-007)：运行时校验器要求子进程
携带 `com.apple.security.app-sandbox` + `com.apple.security.inherit`，而打包契约
(#1052)要求空 entitlements。两者不只是不一致——校验器要的那个形状**根本跑不起来**：
父进程(Runtime Broker)不是 sandboxed 的，子进程声明 inherit 会在
`_libsecinit_appsandbox` 里以「Process is not in an inherited sandbox」中止，
**在 `main` 之前**。最后以修改校验器收口，spec/ADR 对齐留白。

`arkforged` 是一个新的 Rust daemon，外加一个捆绑的固定哈希 vendor tool，
会原样踏进同一片区域。本文是那份留白的对齐。

另一条同样已经付过代价的事实是 AD-015 / AD-011：**字节相等不等于能用**。
同一份 `bbd7bdc0…` 的 `rkdeveloptool`，带 `com.apple.quarantine` 时挂死在 dyld，
摘要检查一切正常，栈全部停在 `_dyld_start`，从未进入 `main`。

两条教训指向同一句话，只是层次不同：**这些字节起不来**。
AD-007 是 entitlement 让它在 `main` 之前中止；AD-015 是 Gatekeeper 让它在 dyld 里停住。
本契约把两条都写成机器可判的条款。

## 决定

### 1. 投放形状：arkforged 不要自己的 LaunchAgent

`arkforged` 由 `arkdeck-agentd` **spawn**，pairing secret 走它继承的 stdin
(`--pair-from-stdin`，architecture.md 15.2)。因此：

- 它是 `ArkDeckAgent.app` 内的 **nested code**，与 `rkdeveloptool` 同级；
- 它**不**注册第二个 LaunchAgent、不做 login item、不做 LaunchDaemon、
  不做 privileged helper、不申请 root。#1299 之后 ArkDeck 只有一个用户级
  LaunchAgent(`com.arkdeck.agentd`)，本契约不增加第二个；
- 它不搜索 PATH、不接受调用方给的可执行路径，工作目录由 `--runtime-dir` 固定
  (与 ArkDeck ADR-0003 的 `RockchipToolRuntime` 同一条理由：上游工具会在
  cwd 旁边写 `log/`，不能让它写进谁碰巧启动了 daemon 的那个目录)。

具体落点(`Contents/Helpers/` 还是别处)是容器那侧的契约，属 ArkDeck 变更；
本仓声明的是**性质**，不是路径。

### 2. Entitlement：两个二进制都是空字典

`arkforged` 与它绑定的 vendor tool，entitlement 字典都必须为空——
`packaging/macos/arkforged.entitlements`、
`packaging/macos/rkdeveloptool.entitlements`，都是 `<dict/>`。

空不是省事，空是机制：

- **App Sandbox 不能有**。父进程不是 sandboxed 的，`inherit` 没有可继承的对象，
  声明它就是 AD-007 的原样复现；
- **其他键也不需要**。USB 能到达工具，是因为两个进程都不在 sandbox 里，
  不是因为谁获得了 capability。`device.usb` 是 App Sandbox 的词汇，
  在一个非 sandboxed 进程上写它既无作用也无意义；
- **`keychain-access-groups` 不能有**。`arkforged` 没有静态秘密：pairing secret
  从 stdin 来、只在内存、不落盘明文(architecture.md 15.2)。声明 Keychain 组
  等于声称一个它没有的资产;
- **Hardened Runtime 例外一律不能有**。`disable-library-validation`、
  `allow-jit`、`allow-unsigned-executable-memory`、`allow-dyld-environment-variables`、
  `disable-executable-page-protection`、`debugger` 都会取消「跑的就是签的」这条保证;
- **`get-task-allow` 不能有**。它让本机任何同用户进程可以 attach 到一个写分区的进程上。

判定用「字典为空」而不是 denylist——为空严格强于任何禁用清单，
也与 ArkDeck 现在的校验器语义一字不差(`entitlements == nil || isEmpty`)。
denylist 只用来生成诊断句子：看到 `app-sandbox` 的人应该被直接告知它在
`main` 之前就中止了，而不是自己去翻 crash report。

### 3. 签名：Developer ID、Hardened Runtime、安全时间戳、由内向外

- 一个 Team 的 Developer ID Application 证书，`--options runtime --timestamp`；
- signing identifier 取容器前缀加组件名：`com.arkdeck.agentd.arkforged`、
  `com.arkdeck.agentd.rkdeveloptool`(与 ArkDeck 的
  `com.arkdeck.desktop.rkdeveloptool` 同一套命名)；
- **由内向外逐个签**，`codesign --deep` 永不用于签名；`--deep --strict` 只用于
  最后的只读验证。这与 #1299 的 `build-helpers.sh` 是同一条顺序；
- **动态库闭包必须只有系统库**。`/usr/lib` 与 `/System/Library` 之外的任何
  依赖都拒绝——一个 release 组件不能依赖某台机器碰巧装了什么。

### 4. 公证：本仓不提交，随容器一起公证

nested code 的公证票据由**最外层容器**承载。ArkDeck 的 packager 提交它自己的
归档、staple、`spctl --assess`；本仓的 packager 产出**已签名的 nested code 加一张
receipt**，不单独提交公证——单独提交只会得到一张没有东西去 staple 的票据。

### 5. 运行期强制：谁验证谁

**父验证子。** 这条是本契约的骨架：

~~~text
arkdeck-agentd  --安装时验证 Developer ID/Team/bundle ID/hardened runtime/
                  embedded profile，之后每次 spawn 前重验文件身份(#1299)-->  arkforged
arkforged       --绑定前读 Mach-O 代码签名，判 entitlement 与签名形状-->      rkdeveloptool
~~~

进程读自己的签名讲给自己听，不构成任何证明，所以 `arkforged` 不做自证。

`crates/arkforged/src/packaging.rs` 是 ArkForge 这一侧的实现。两个模式：

| 模式 | 强制 | 用途 |
|---|---|---|
| Development(默认) | entitlement 字典必须为空 | 本地构建。2026-08-15 彩排跑的那份工具是 ad-hoc linker-signed，拒绝它等于拒绝唯一驱动过这块板子的二进制 |
| Release(`--require-release-signing`) | 上述 + 已签名、非 ad-hoc、Hardened Runtime、有 Team ID | 出厂形状 |

**entitlement 条款在两个模式下都强制**，因为不存在「这个构建里 sandbox 子进程能跑」
的情况。签名条款只在 Release 强制。

fat 二进制的**每一个** slice 都判，理由与 ArkDeck 传
`kSecCSCheckAllArchitectures` 相同：第二个 slice 带着 App Sandbox 键的二进制，
是在别人机器上失败、不在打包者机器上失败的二进制。

### 6. 本契约刻意不做身份检查

`--rkdeveloptool-sha256` 已经逐字节钉死了身份，**严格强于**任何 signing identifier
或 Team ID 比对。签名在字节钉之上补的不是「这是哪个二进制」，而是
**macOS 会不会让它起来**——AD-011 就是一份摘要完美、因带 quarantine 而挂死在 dyld 的二进制。

所以 Release 条款问的是「能不能被公证、会不会被 Gatekeeper 挡住」，
不问谁的名字签在上面。这也让本仓不必把某一个 authority 的 Team ID 编进 daemon。

同样地，daemon **不做 Gatekeeper 评估**：`spctl --assess` 可能走网络，
daemon 启动不是做这件事的地方。公证与 staple 由 packager 验一次。

### 7. 打包顺序 fail-closed

`packaging/macos/package-arkforged.sh`，顺序固定，任一阶段不得跳过或换序：

~~~text
未签名输入逐字节核对(含 symlink 拒绝)
  -> release 构建
  -> 架构与动态库闭包检查
  -> 由内向外逐个签名，各带自己的空 entitlements
  -> codesign --verify --strict 独立回读
  -> codesign -d --entitlements 独立回读
  -> 仓内 reader 二次判定(arkforge-signing --release)
  -> 已签名字节的摘要写入 receipt
~~~

自报字段永不替代检查：每一条性质都从**已签名的字节**里读回来，
而不是从提出要求的那个参数上继承下来。两个 reader 同时存在是有意的——
`codesign` 是系统的答案，仓内 reader 是 daemon 绑定时真正会应用的那个；
只有一边强制的契约是会漂的契约。

## 实测(2026-08-16，本机 macOS 26)

仓内 reader 在五个真实二进制上逐条复现了 `codesign` 的答案：

| 二进制 | reader 读到 | 与 `codesign` 一致 |
|---|---|---|
| `~/dayu200-rehearsal/.../rkdeveloptool`(彩排用，= ArkDeck `pinnedProduction` `038a8a0e…`) | arm64, linker-signed, 无 team, 无 entitlement | ✅ `flags=0x20002(adhoc,linker-signed)`、`TeamIdentifier=not set` |
| `/Applications/ArkDeck.app/Contents/MacOS/rkdeveloptool`(`231a05ef…`) | arm64, runtime, team `8AQTYW5FKR`, 无 entitlement | ✅ 且 XML 与 DER 两个 slot 都是空字典 |
| `/usr/bin/codesign` | fat，两个 slice 各 1 条 entitlement | ✅ |
| `Calculator` | fat，各 15 条，含 `com.apple.security.app-sandbox` | ✅ 致命键被判出 |
| `/Applications/ArkDeck.app/Contents/MacOS/ArkDeck` | 7 条 entitlement | ✅ 与 `ArkDeckApp.entitlements` 源文件逐条相同 |

packager 与 daemon 的完整闭环也跑通了：真实 Developer ID 签名 → 独立回读 →
receipt → 已签名的 daemon 用 `--require-release-signing` 绑定已签名的工具：

~~~text
dispatch: …/rkdeveloptool (8048ad969bf789e0…)
  signing: arm64 com.arkdeck.agentd.rkdeveloptool (runtime, team 8AQTYW5FKR, no entitlements)
  self-test: rkdeveloptool ver 1.32 in 398 ms
execution: not ready (NO_PAIRED_AUTHORITY)
~~~

### 顺带查出来的一件事(AD-023)

第一次跑 packager 时它拒绝了彩排用的那份工具，理由是动态库闭包：

~~~text
rkdeveloptool links /opt/homebrew/opt/libusb/lib/libusb-1.0.0.dylib,
which is not a system library
~~~

这不是误报。**2026-08-15 彩排使用的 `038a8a0e…` 是一个不可出厂的构建**——
它依赖 Homebrew 在这台机器上的 libusb 路径。ArkDeck 自己的密闭可复现构建
(`231a05ef…`，App 内捆绑的那份)把 libusb 静态链进去了，闭包只剩系统库，
是三份里唯一满足本契约的一份。详见 ledger AD-023。

## 代价与边界

- **公证从未真正做过。** 本仓没有提交过 notarization，也没有做过 staple 或
  干净主机上的 Gatekeeper 验收。Release 条款检的是「可被公证的形状」，
  不是「已被公证的事实」。真正的公证在容器那侧，属 ArkDeck 变更；
- **daemon 不检 staple。** 见上，理由是启动路径不该走网络。一个已 staple 的票据
  与一个没有的，在 daemon 眼里一样；
- **只覆盖 macOS。** Windows named pipe 是设计预留(architecture.md 15.2)，
  Linux 无对应机制。本契约不假装覆盖它们；
- **reader 不验证签名的密码学有效性。** 它读 CodeDirectory 的事实(identifier、
  team、flags、entitlement)，不校验 CMS 链——那是 `codesign --verify` 的工作，
  packager 会调用它。仓内 reader 是第二意见，不是替代品；
- **Mach-O 大端不解码**，直接拒绝。对一个二进制什么都不说，不等于说它是干净的。

## 复核条件

出现以下任一情况时重开此决定：

- **仓内出现进程内 USB 后端**(AF-V4 / Unisoc 方向)。那时 `arkforged` 自己要碰
  IOKit，「空 entitlement」不再显然正确，USB 与 sandbox 的问题会整套回来；
- ArkDeck 改变 helper 的投放形状(例如把 agent 从 LaunchAgent 改成别的)，
  本契约第 1 节随之失效；
- Apple 改变 nested code 的公证或 entitlement 语义；
- 需要在 macOS 之外出厂。

## NRU-004 后续状态（2026-08-19）

原生 RockUSB 现为唯一执行端口，发布包只携带并签名 `arkforged`。vendor
签名、自检、entitlement、显式端口选择与迁移 fallback 的 runtime lane 均已
删除；本文此前记录的 vendor 条款只作为历史实测保留，不描述任何当前可调用或
可恢复的实现。本附录取代文首 2026-08-18 修订中“仍保留 migration fallback”
的状态描述。
