import CryptoKit
import Foundation

public enum ArkForgeBundleMemberRole: String, Codable, Sendable {
  case cli
  case daemon
  case profile
}

public struct ArkForgeBundleMember: Codable, Sendable, Equatable {
  public let path: String
  public let sha256: String
  public let bytes: UInt64
  public let role: ArkForgeBundleMemberRole
  public let profileID: String?

  enum CodingKeys: String, CodingKey {
    case path, sha256, bytes, role
    case profileID = "profileId"
  }

  public init(
    path: String, sha256: String, bytes: UInt64,
    role: ArkForgeBundleMemberRole, profileID: String? = nil
  ) {
    self.path = path
    self.sha256 = sha256
    self.bytes = bytes
    self.role = role
    self.profileID = profileID
  }
}

public struct ArkForgeBundleManifest: Codable, Sendable, Equatable {
  public static let currentSchema = "arkforge.release-bundle/v1"
  public static let relativePath = "Contents/Resources/arkforge-bundle.json"

  public let schema: String
  public let version: String
  public let members: [ArkForgeBundleMember]

  public init(
    schema: String = Self.currentSchema, version: String,
    members: [ArkForgeBundleMember]
  ) {
    self.schema = schema
    self.version = version
    self.members = members
  }

  public func canonicalJSON() throws -> Data {
    let encoder = JSONEncoder()
    encoder.outputFormatting = [.sortedKeys, .withoutEscapingSlashes]
    return try encoder.encode(self)
  }
}

public struct ArkForgeBundleMemberDeclaration: Sendable, Equatable {
  public let path: String
  public let role: ArkForgeBundleMemberRole
  public let profileID: String?

  public init(
    path: String, role: ArkForgeBundleMemberRole, profileID: String? = nil
  ) {
    self.path = path
    self.role = role
    self.profileID = profileID
  }
}

public struct ArkForgeReleaseBundle: Sendable, Equatable {
  public let rootURL: URL
  public let manifestURL: URL
  public let manifestSHA256: String
  public let version: String
  public let cliURL: URL
  public let daemonURL: URL
  public let profileURLs: [String: URL]
}

public enum ArkForgeReleaseBundleError: Error, Equatable, CustomStringConvertible {
  case filesystem(String)
  case malformedManifest(String)
  case unsupportedSchema(String)
  case unsafePath(String)
  case symbolicLink(String)
  case nonRegularMember(String)
  case undeclaredMember(String)
  case missingMember(String)
  case duplicatePath(String)
  case duplicateRole(String)
  case duplicateProfileID(String)
  case invalidRole(String)
  case invalidDigest(path: String, value: String)
  case sizeMismatch(path: String, expected: UInt64, actual: UInt64)
  case digestMismatch(path: String, expected: String, actual: String)

  public var description: String {
    switch self {
    case .filesystem(let detail): return detail
    case .malformedManifest(let detail): return "malformed ArkForge bundle manifest: \(detail)"
    case .unsupportedSchema(let schema): return "unsupported ArkForge bundle schema \(schema)"
    case .unsafePath(let path): return "unsafe ArkForge bundle member path: \(path)"
    case .symbolicLink(let path): return "ArkForge bundle member is a symbolic link: \(path)"
    case .nonRegularMember(let path): return "ArkForge bundle member is not a regular file: \(path)"
    case .undeclaredMember(let path): return "ArkForge bundle contains undeclared member: \(path)"
    case .missingMember(let path): return "ArkForge bundle is missing declared member: \(path)"
    case .duplicatePath(let path): return "ArkForge bundle declares duplicate path: \(path)"
    case .duplicateRole(let role): return "ArkForge bundle declares duplicate role: \(role)"
    case .duplicateProfileID(let id): return "ArkForge bundle declares duplicate profile id: \(id)"
    case .invalidRole(let detail): return "invalid ArkForge bundle role: \(detail)"
    case .invalidDigest(let path, let value):
      return "ArkForge bundle member \(path) has invalid SHA-256 \(value)"
    case .sizeMismatch(let path, let expected, let actual):
      return "ArkForge bundle member \(path) is \(actual) bytes, expected \(expected)"
    case .digestMismatch(let path, let expected, let actual):
      return "ArkForge bundle member \(path) has SHA-256 \(actual), expected \(expected)"
    }
  }
}

public enum ArkForgeBundleManifestWriter {
  @discardableResult
  public static func write(
    bundleURL: URL, version: String,
    declarations: [ArkForgeBundleMemberDeclaration]
  ) throws -> ArkForgeBundleManifest {
    var members: [ArkForgeBundleMember] = []
    for declaration in declarations.sorted(by: { $0.path < $1.path }) {
      let url = try ArkForgeReleaseBundleReader.validatedMemberURL(
        rootURL: bundleURL, relativePath: declaration.path)
      let facts = try ArkForgeReleaseBundleReader.regularFileFacts(
        url: url, relativePath: declaration.path)
      members.append(
        ArkForgeBundleMember(
          path: declaration.path, sha256: facts.sha256, bytes: facts.bytes,
          role: declaration.role, profileID: declaration.profileID))
    }
    let manifest = ArkForgeBundleManifest(version: version, members: members)
    let manifestURL = bundleURL.appendingPathComponent(ArkForgeBundleManifest.relativePath)
    try FileManager.default.createDirectory(
      at: manifestURL.deletingLastPathComponent(),
      withIntermediateDirectories: true)
    try manifest.canonicalJSON().write(to: manifestURL, options: [.atomic])
    _ = try ArkForgeReleaseBundleReader.load(bundleURL: bundleURL)
    return manifest
  }
}

public enum ArkForgeReleaseBundleReader {
  public static func load(bundleURL: URL) throws -> ArkForgeReleaseBundle {
    let requestedRoot = bundleURL.standardizedFileURL
    let rootValues = try resourceValues(requestedRoot)
    guard rootValues.isSymbolicLink != true else {
      throw ArkForgeReleaseBundleError.symbolicLink(requestedRoot.path)
    }
    guard rootValues.isDirectory == true else {
      throw ArkForgeReleaseBundleError.filesystem(
        "ArkForge bundle root is not a directory: \(requestedRoot.path)")
    }
    // FileManager can enumerate `/private/var/...` for a caller-provided
    // `/var/...` URL. Resolve ancestor aliases once so relative paths are
    // derived from one filesystem spelling; the bundle entry itself was
    // checked above and still may not be a symlink.
    let root = requestedRoot.resolvingSymlinksInPath().standardizedFileURL

    let manifestURL = root.appendingPathComponent(ArkForgeBundleManifest.relativePath)
    let manifestFacts = try regularFileFacts(
      url: manifestURL, relativePath: ArkForgeBundleManifest.relativePath)
    let manifestData: Data
    do {
      manifestData = try Data(contentsOf: manifestURL, options: [.mappedIfSafe])
    } catch {
      throw ArkForgeReleaseBundleError.filesystem(
        "cannot read ArkForge bundle manifest: \(error)")
    }
    let manifest: ArkForgeBundleManifest
    do {
      manifest = try JSONDecoder().decode(ArkForgeBundleManifest.self, from: manifestData)
    } catch {
      throw ArkForgeReleaseBundleError.malformedManifest(String(describing: error))
    }
    guard manifest.schema == ArkForgeBundleManifest.currentSchema else {
      throw ArkForgeReleaseBundleError.unsupportedSchema(manifest.schema)
    }
    guard !manifest.version.isEmpty else {
      throw ArkForgeReleaseBundleError.malformedManifest("version is empty")
    }

    var declaredPaths = Set<String>()
    var cliURL: URL?
    var daemonURL: URL?
    var profiles: [String: URL] = [:]
    for member in manifest.members {
      guard declaredPaths.insert(member.path).inserted else {
        throw ArkForgeReleaseBundleError.duplicatePath(member.path)
      }
      guard isLowercaseSHA256(member.sha256) else {
        throw ArkForgeReleaseBundleError.invalidDigest(path: member.path, value: member.sha256)
      }
      let url = try validatedMemberURL(rootURL: root, relativePath: member.path)
      let facts = try regularFileFacts(url: url, relativePath: member.path)
      guard facts.bytes == member.bytes else {
        throw ArkForgeReleaseBundleError.sizeMismatch(
          path: member.path, expected: member.bytes, actual: facts.bytes)
      }
      guard facts.sha256 == member.sha256 else {
        throw ArkForgeReleaseBundleError.digestMismatch(
          path: member.path, expected: member.sha256, actual: facts.sha256)
      }
      switch member.role {
      case .cli:
        guard member.path == "Contents/MacOS/arkforge", member.profileID == nil else {
          throw ArkForgeReleaseBundleError.invalidRole(member.path)
        }
        guard cliURL == nil else { throw ArkForgeReleaseBundleError.duplicateRole("cli") }
        cliURL = url
      case .daemon:
        guard member.path == "Contents/MacOS/arkforged", member.profileID == nil else {
          throw ArkForgeReleaseBundleError.invalidRole(member.path)
        }
        guard daemonURL == nil else { throw ArkForgeReleaseBundleError.duplicateRole("daemon") }
        daemonURL = url
      case .profile:
        guard member.path.hasPrefix("Contents/Resources/profiles/"),
          let profileID = member.profileID, !profileID.isEmpty
        else {
          throw ArkForgeReleaseBundleError.invalidRole(member.path)
        }
        guard profiles.updateValue(url, forKey: profileID) == nil else {
          throw ArkForgeReleaseBundleError.duplicateProfileID(profileID)
        }
      }
    }

    guard let cliURL else { throw ArkForgeReleaseBundleError.missingMember("cli") }
    guard let daemonURL else { throw ArkForgeReleaseBundleError.missingMember("daemon") }
    guard !profiles.isEmpty else { throw ArkForgeReleaseBundleError.missingMember("profile") }
    try rejectUndeclaredMembers(rootURL: root, declaredPaths: declaredPaths)

    return ArkForgeReleaseBundle(
      rootURL: root, manifestURL: manifestURL,
      manifestSHA256: manifestFacts.sha256, version: manifest.version,
      cliURL: cliURL, daemonURL: daemonURL, profileURLs: profiles)
  }

  static func validatedMemberURL(rootURL: URL, relativePath: String) throws -> URL {
    let components = relativePath.split(separator: "/", omittingEmptySubsequences: false)
    guard !relativePath.isEmpty, !relativePath.hasPrefix("/"), !relativePath.contains("\\"),
      components.allSatisfy({ !$0.isEmpty && $0 != "." && $0 != ".." })
    else {
      throw ArkForgeReleaseBundleError.unsafePath(relativePath)
    }
    let root = rootURL.standardizedFileURL
    let candidate = root.appendingPathComponent(relativePath).standardizedFileURL
    let resolvedRoot = root.resolvingSymlinksInPath().path
    let resolvedCandidate = candidate.resolvingSymlinksInPath().path
    guard resolvedCandidate.hasPrefix(resolvedRoot + "/") else {
      throw ArkForgeReleaseBundleError.unsafePath(relativePath)
    }
    return candidate
  }

  static func regularFileFacts(url: URL, relativePath: String) throws
    -> (bytes: UInt64, sha256: String)
  {
    let values = try resourceValues(url)
    guard values.isSymbolicLink != true else {
      throw ArkForgeReleaseBundleError.symbolicLink(relativePath)
    }
    guard values.isRegularFile == true else {
      if !FileManager.default.fileExists(atPath: url.path) {
        throw ArkForgeReleaseBundleError.missingMember(relativePath)
      }
      throw ArkForgeReleaseBundleError.nonRegularMember(relativePath)
    }
    let size = UInt64(values.fileSize ?? 0)
    return (size, try sha256(url))
  }

  private static func rejectUndeclaredMembers(
    rootURL: URL, declaredPaths: Set<String>
  ) throws {
    let subpaths: [String]
    do {
      // This API returns paths relative to the directory. URL enumeration can
      // spell the same temporary ancestor as `/private/var` while the input
      // URL says `/var`, which makes character-prefix subtraction unsafe.
      subpaths = try FileManager.default.subpathsOfDirectory(atPath: rootURL.path)
    } catch {
      throw ArkForgeReleaseBundleError.filesystem(
        "cannot enumerate ArkForge bundle: \(error)")
    }
    for relative in subpaths {
      let url = rootURL.appendingPathComponent(relative)
      let values = try resourceValues(url)
      if values.isSymbolicLink == true {
        throw ArkForgeReleaseBundleError.symbolicLink(relative)
      }
      if values.isDirectory == true { continue }
      guard values.isRegularFile == true else {
        throw ArkForgeReleaseBundleError.nonRegularMember(relative)
      }
      if relative == ArkForgeBundleManifest.relativePath { continue }
      guard declaredPaths.contains(relative) else {
        throw ArkForgeReleaseBundleError.undeclaredMember(relative)
      }
    }
  }

  private static func resourceValues(_ url: URL) throws -> URLResourceValues {
    do {
      return try url.resourceValues(forKeys: [
        .isRegularFileKey, .isDirectoryKey, .isSymbolicLinkKey, .fileSizeKey,
      ])
    } catch {
      throw ArkForgeReleaseBundleError.filesystem("cannot inspect \(url.path): \(error)")
    }
  }

  private static func isLowercaseSHA256(_ value: String) -> Bool {
    value.count == 64 && value == value.lowercased()
      && value.utf8.allSatisfy { byte in
        (byte >= 48 && byte <= 57) || (byte >= 97 && byte <= 102)
      }
  }

  private static func sha256(_ url: URL) throws -> String {
    let handle: FileHandle
    do {
      handle = try FileHandle(forReadingFrom: url)
    } catch {
      throw ArkForgeReleaseBundleError.filesystem("cannot open \(url.path): \(error)")
    }
    defer { try? handle.close() }
    var hasher = SHA256()
    do {
      while true {
        let chunk = try handle.read(upToCount: 1 << 20) ?? Data()
        if chunk.isEmpty { break }
        hasher.update(data: chunk)
      }
    } catch {
      throw ArkForgeReleaseBundleError.filesystem("cannot read \(url.path): \(error)")
    }
    return hasher.finalize().map { String(format: "%02x", $0) }.joined()
  }
}
