// swift-tools-version: 6.0

import PackageDescription

let package = Package(
  name: "ArkForgeSDK",
  platforms: [.macOS(.v13)],
  products: [
    .library(name: "ArkForgeProtocol", targets: ["ArkForgeProtocol"]),
    .library(name: "ArkForgeClient", targets: ["ArkForgeClient"]),
  ],
  targets: [
    .target(
      name: "ArkForgeProtocol",
      path: "swift/ArkForgeSDK/Sources/ArkForgeProtocol"),
    .target(
      name: "ArkForgeClient",
      dependencies: ["ArkForgeProtocol"],
      path: "swift/ArkForgeSDK/Sources/ArkForgeClient"),
    .testTarget(
      name: "ArkForgeProtocolTests",
      dependencies: ["ArkForgeProtocol"],
      path: "swift/ArkForgeSDK/Tests/ArkForgeProtocolTests"),
    .testTarget(
      name: "ArkForgeClientTests",
      dependencies: ["ArkForgeClient", "ArkForgeProtocol"],
      path: "swift/ArkForgeSDK/Tests/ArkForgeClientTests"),
  ]
)
