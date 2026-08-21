import Foundation
import XCTest

@testable import ArkForgeClient

final class ArkForgeReleaseBundleTests: XCTestCase {
  private var temporaryRoots: [URL] = []

  override func tearDownWithError() throws {
    for root in temporaryRoots {
      try? FileManager.default.removeItem(at: root)
    }
    temporaryRoots.removeAll()
  }

  private func makeBundle() throws -> URL {
    let root = FileManager.default.temporaryDirectory
      .appendingPathComponent("ArkForgeBundleTests-\(UUID().uuidString)")
      .appendingPathComponent("ArkForge.bundle")
    temporaryRoots.append(root.deletingLastPathComponent())
    let executableDirectory = root.appendingPathComponent("Contents/MacOS")
    let profilesDirectory = root.appendingPathComponent("Contents/Resources/profiles")
    try FileManager.default.createDirectory(
      at: executableDirectory, withIntermediateDirectories: true)
    try FileManager.default.createDirectory(
      at: profilesDirectory, withIntermediateDirectories: true)
    try Data("cli".utf8).write(to: executableDirectory.appendingPathComponent("arkforge"))
    try Data("daemon".utf8).write(to: executableDirectory.appendingPathComponent("arkforged"))
    try Data("profile".utf8).write(
      to: profilesDirectory.appendingPathComponent("dayu200.yaml"))
    return root
  }

  private func declarations() -> [ArkForgeBundleMemberDeclaration] {
    [
      ArkForgeBundleMemberDeclaration(
        path: "Contents/MacOS/arkforge", role: .cli),
      ArkForgeBundleMemberDeclaration(
        path: "Contents/MacOS/arkforged", role: .daemon),
      ArkForgeBundleMemberDeclaration(
        path: "Contents/Resources/profiles/dayu200.yaml", role: .profile,
        profileID: "org.openharmony.dayu200"),
    ]
  }

  func testOneManifestPathComposesTheWholeRelease() throws {
    let root = try makeBundle()
    let manifest = try ArkForgeBundleManifestWriter.write(
      bundleURL: root, version: "0.1.0", declarations: declarations())
    let bundle = try ArkForgeReleaseBundleReader.load(bundleURL: root)

    XCTAssertEqual(manifest.schema, ArkForgeBundleManifest.currentSchema)
    XCTAssertEqual(bundle.version, "0.1.0")
    XCTAssertEqual(bundle.cliURL.lastPathComponent, "arkforge")
    XCTAssertEqual(bundle.daemonURL.lastPathComponent, "arkforged")
    XCTAssertEqual(
      bundle.profileURLs["org.openharmony.dayu200"]?.lastPathComponent,
      "dayu200.yaml")
    XCTAssertEqual(bundle.manifestSHA256.count, 64)
  }

  func testDigestDriftFailsClosed() throws {
    let root = try makeBundle()
    try ArkForgeBundleManifestWriter.write(
      bundleURL: root, version: "0.1.0", declarations: declarations())
    try Data("different daemon".utf8).write(
      to: root.appendingPathComponent("Contents/MacOS/arkforged"))

    XCTAssertThrowsError(try ArkForgeReleaseBundleReader.load(bundleURL: root)) { error in
      guard case ArkForgeReleaseBundleError.sizeMismatch = error else {
        return XCTFail("expected size refusal before digest use, got \(error)")
      }
    }
  }

  func testUndeclaredFileFailsClosed() throws {
    let root = try makeBundle()
    try ArkForgeBundleManifestWriter.write(
      bundleURL: root, version: "0.1.0", declarations: declarations())
    try Data("shadow".utf8).write(
      to: root.appendingPathComponent("Contents/MacOS/arkforged-shadow"))

    XCTAssertThrowsError(try ArkForgeReleaseBundleReader.load(bundleURL: root)) { error in
      XCTAssertEqual(
        error as? ArkForgeReleaseBundleError,
        .undeclaredMember("Contents/MacOS/arkforged-shadow"))
    }
  }

  func testSymlinkAndTraversalFailClosed() throws {
    let root = try makeBundle()
    try ArkForgeBundleManifestWriter.write(
      bundleURL: root, version: "0.1.0", declarations: declarations())
    let daemon = root.appendingPathComponent("Contents/MacOS/arkforged")
    try FileManager.default.removeItem(at: daemon)
    try FileManager.default.createSymbolicLink(
      at: daemon, withDestinationURL: root.appendingPathComponent("Contents/MacOS/arkforge"))
    XCTAssertThrowsError(try ArkForgeReleaseBundleReader.load(bundleURL: root)) { error in
      XCTAssertEqual(
        error as? ArkForgeReleaseBundleError,
        .symbolicLink("Contents/MacOS/arkforged"))
    }

    XCTAssertThrowsError(
      try ArkForgeBundleManifestWriter.write(
        bundleURL: root, version: "0.1.0",
        declarations: [
          ArkForgeBundleMemberDeclaration(path: "../outside", role: .daemon)
        ])
    ) { error in
      XCTAssertEqual(error as? ArkForgeReleaseBundleError, .unsafePath("../outside"))
    }
  }
}
