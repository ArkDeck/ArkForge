# ArkForge

设备无关的刷机机械层(Rust)。把固件容器解析、芯片下载协议、USB transport、分区擦写/校验与厂商工具语义收进一个独立 daemon(`arkforged`)，ArkDeck 只保留 authority。

~~~text
ArkDeck 决定：谁、对哪台设备、以哪个已发布 Operation、在什么安全边界下执行。

ArkForge 决定：该已授权语义计划如何通过具体固件格式、Provider 和 Transport 正确落地。
~~~

## 文档

- 架构正本：[docs/architecture.md](docs/architecture.md)(状态 Proposed；ArkDeck 审计基线 `2849c5c1`)
- 任务台账：[TASKS.md](TASKS.md)(AF-V1~V4 四个垂直任务，全部未开工)

## 目标设备

- DAYU200(Rockchip RK3568 / RockUSB)：首个生产垂直，首版封装固定哈希 rkdeveloptool；
- DAYU600(Unisoc uis7885 / PAC)：证据门(architecture.md 17.5)通过前仅 inspect 与非可执行 PlanAssessment。

## 与 ArkDeck 的关系

- 经 `arkforge-arkdeck-adapter` 接入；Core 不依赖 ArkDeck 类型；
- ArkDeck Runtime 保留唯一 authority(admission / RuntimeCapability / device lane / intent)；
- 每个 mutation/destructive action 需要 exact StepPermit；outcomeUnknown 永不 replay；
- 新 Operation/Provider/Profile 属 ArkDeck 明确要求 review 的变更，与真实产品能力同车交付。

## 命名

原案名 ArkFlash；2026-08-14 定名 ArkForge。ArkFlash 名称保留给未来面向用户的刷机 UI 产品位。
