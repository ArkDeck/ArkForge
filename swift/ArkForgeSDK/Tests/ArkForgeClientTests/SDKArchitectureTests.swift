import Foundation
import XCTest

@testable import ArkForgeClient
@testable import ArkForgeProtocol

final class SDKArchitectureTests: XCTestCase {
  func testSDKHasNoArkDeckModuleDependency() throws {
    let packageRoot = URL(fileURLWithPath: #filePath)
      .deletingLastPathComponent()
      .deletingLastPathComponent()
      .deletingLastPathComponent()
    let sources = packageRoot.appendingPathComponent("Sources")
    let enumerator = try XCTUnwrap(
      FileManager.default.enumerator(at: sources, includingPropertiesForKeys: nil))
    for case let url as URL in enumerator where url.pathExtension == "swift" {
      let source = try String(contentsOf: url, encoding: .utf8)
      XCTAssertFalse(source.contains("import ArkDeck"), "\(url.lastPathComponent) imports ArkDeck")
      XCTAssertFalse(source.contains("RuntimeCapability"), "SDK must not own runtime authority")
      XCTAssertFalse(source.contains("safeToSupersede"), "SDK must not classify recovery")
    }
  }

  func testPublicAndControllerClientsAreDistinctTypes() {
    XCTAssertNotEqual(
      String(reflecting: ArkForgePublicClient.self),
      String(reflecting: ArkForgeControllerClient.self))
    XCTAssertEqual(ArkForgeFraming.maxFrameBytes, 16 * 1024 * 1024)
  }
}
