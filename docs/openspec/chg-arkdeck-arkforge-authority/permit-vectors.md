# StepPermit 交叉验证向量

支撑 `AFA-AC-2`。ArkDeck 的 Swift 实现必须对同样的输入产出同样的
canonical CBOR signing body 与同样的 HMAC-SHA256 tag。

生成方：ArkForge `crates/arkforge-authority-api/tests/permit_vectors.rs`。
那个测试同时是这份文档的守卫——编码一变，测试就红。

## 公共参数

| 项 | 值 |
|---|---|
| pairing secret | ASCII `arkforge-arkdeck-permit-vector-secret`（37 字节，无 NUL 终止） |
| `PairingEpoch` | `1` |
| MAC | HMAC-SHA256(secret, signingBody) |
| 编码 | RFC 8949 §4.2.1 确定性编码；map key 按**编码后字节**排序；无浮点、无 tag、无不定长 |

## 每条向量的固定字段

~~~text
authorityNamespace          "arkdeck"
controllerSessionId         "SESSION-VECTOR"
jobId                       "JOB-VECTOR"
planId                      "PLAN-VECTOR"
planDigest                  SHA-256("plan-vector")
publicStepDigest            SHA-256("public-step-vector")
effectSetDigest             SHA-256("effect-set-vector")
authorityBinding.namespace  "arkdeck"
authorityBinding.bindingId  "BINDING-VECTOR"
authorityBinding.revision   3
authorityBinding.identity   SHA-256("stable-identity-vector")
admittedDeviceFactsDigest   SHA-256("admitted-facts-vector")
issuedAtEpochMs             1770000000000
expiresAtEpochMs            1770000060000
singleUse                   true
permitId                    "PERMIT-" + stepId
~~~

`signingBody` **不含** `integrityTag`——tag 覆盖 body，body 不覆盖 tag。

## 向量

| # | stepId | attemptId | privateActionDigest 的原像 | SHA-256(signingBody) | tag |
|---:|---|---|---|---|---|
| 1 | `STEP-ENSURE-MODE` | `ATTEMPT-1` | `enter-loader` | `bae9c1e8d669e6850eb967524885bed0632b6adbb9de5fb3bea971250fb5cd51` | `d0a4dbc07944f6a802a4f157574f89ddc1cca5f9eb89c7b5c26d99884ea37ae0` |
| 2 | `STEP-WRITE-SYSTEM` | `ATTEMPT-1` | `write-partition:system` | `fbdfcab7a865c5ae6400ab64594c1780e71a06a47da43ab6232674e1cdaa2d2e` | `db38ba9d9a8fbac7840a89b6a9434938b25ed18bc357b4f27e39751d21be1523` |
| 3 | `STEP-RESET` | `ATTEMPT-2` | `reset-device` | `cea82597e94d8a47092ef11c7ff91af63e6d9a890292f5407eeeab60960d65f8` | `86805a7585615edaed931ac3ac005e445529ba5de7495292d6fae10e2d9029ec` |

`privateActionDigest` = SHA-256(表中原像的 ASCII 字节)。

## 为什么钉 body 的摘要而不是 body 的字节

body 有几百字节，贴在文档里既难读又易被编辑器改动。摘要不同就说明编码不同，
这正是要检出的东西。如果 Swift 侧对不上，用 ArkForge 那个测试打印完整 body
逐字节比对——差异一般出在 map key 的排序或整数的最短编码上。

## 变更规则

这三条向量变化 = 对任何第二实现的破坏性变更。改动必须与
`CONTROL_TABLE_VERSION` / permit schema 的版本推进同车，不能悄悄改。
