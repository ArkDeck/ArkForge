import Foundation

/// Frame layer: a 4-byte big-endian length followed by that many bytes.
///
/// The length is checked against the limit before allocation. The value must
/// remain byte-for-byte aligned with `arkforge_ipc::wire::MAX_FRAME_BYTES`.
public enum ArkForgeFraming {
  public static let maxFrameBytes = 16 * 1024 * 1024
}

/// The proto3 wire subset `proto/arkforge.proto` uses.
///
/// Hand-written rather than generated, for the same reason ArkForge's side is
/// (AFD-0001): this is the codec on a trust boundary where one side authorizes
/// destructive writes and the other performs them, and both halves should be
/// readable in review without a code generator between them.
///
/// It mirrors `crates/arkforge-ipc/src/wire.rs` field for field. Three rules
/// carry the compatibility contract (IPC-001):
///
/// - a **zero value is not written**, and an absent scalar reads back as zero —
///   proto3 makes "absent" and "default" the same value;
/// - a **nested message is always written**, even when it encodes to nothing,
///   so "present and empty" stays distinguishable from "absent";
/// - an **unknown field is skipped** (forward compatibility) but an **unknown
///   enum value is a hard error**, never a default (ArkForge
///   `architecture.md` 15.2). A daemon that answered with a control action
///   this build has never heard of must not have it silently read as
///   `UNSPECIFIED`.
enum ProtobufWireType: UInt64 {
  case varint = 0
  case fixed64 = 1
  case lengthDelimited = 2
  case fixed32 = 5
}

/// Appends proto3 fields in the encoding ArkForge expects.
struct ProtobufWriter {
  private(set) var bytes: [UInt8] = []

  var data: Data { Data(bytes) }

  mutating func varint(_ value: UInt64) {
    var remaining = value
    repeat {
      var byte = UInt8(remaining & 0x7f)
      remaining >>= 7
      if remaining != 0 { byte |= 0x80 }
      bytes.append(byte)
    } while remaining != 0
  }

  mutating func tag(_ field: UInt32, _ type: ProtobufWireType) {
    varint(UInt64(field) << 3 | type.rawValue)
  }

  mutating func uint64(_ field: UInt32, _ value: UInt64) {
    guard value != 0 else { return }
    tag(field, .varint)
    varint(value)
  }

  mutating func uint32(_ field: UInt32, _ value: UInt32) {
    uint64(field, UInt64(value))
  }

  mutating func bool(_ field: UInt32, _ value: Bool) {
    guard value else { return }
    tag(field, .varint)
    varint(1)
  }

  mutating func enumeration(_ field: UInt32, _ value: Int32) {
    guard value != 0 else { return }
    tag(field, .varint)
    varint(UInt64(value))
  }

  mutating func string(_ field: UInt32, _ value: String) {
    guard !value.isEmpty else { return }
    payload(field, Array(value.utf8))
  }

  mutating func bytes(_ field: UInt32, _ value: [UInt8]) {
    guard !value.isEmpty else { return }
    payload(field, value)
  }

  mutating func bytes(_ field: UInt32, _ value: Data) {
    guard !value.isEmpty else { return }
    payload(field, Array(value))
  }

  /// A nested message, written even when empty — the one place a zero-length
  /// payload is meaningful.
  mutating func message(_ field: UInt32, _ body: [UInt8]) {
    payload(field, body)
  }

  /// A repeated string, one field entry each. Empty entries are written:
  /// dropping one would change the list's length, which is data.
  mutating func repeatedString(_ field: UInt32, _ values: [String]) {
    for value in values {
      payload(field, Array(value.utf8))
    }
  }

  private mutating func payload(_ field: UInt32, _ value: [UInt8]) {
    tag(field, .lengthDelimited)
    varint(UInt64(value.count))
    bytes.append(contentsOf: value)
  }
}

/// Why a message could not be read.
public enum ProtobufWireError: Error, Equatable, CustomStringConvertible {
  case truncated
  case malformedVarint
  case unknownWireType(UInt64)
  case tooDeep
  case invalidUTF8(field: UInt32)
  /// Fail-closed by design: a value this build does not know is not a default.
  case unknownEnumValue(message: String, field: UInt32, value: Int64)
  case missingField(message: String, field: UInt32)
  case frameTooLarge(Int)

  public var description: String {
    switch self {
    case .truncated: return "message ended mid-field"
    case .malformedVarint: return "varint is longer than ten bytes"
    case .unknownWireType(let value): return "unknown wire type \(value)"
    case .tooDeep: return "nested messages exceed the depth bound"
    case .invalidUTF8(let field): return "field \(field) is not valid UTF-8"
    case .unknownEnumValue(let message, let field, let value):
      return
        "\(message) field \(field) carries unknown enum value \(value); an unknown enum is a "
        + "refusal, not a default"
    case .missingField(let message, let field):
      return "\(message) is missing required field \(field)"
    case .frameTooLarge(let size):
      return "frame of \(size) bytes exceeds the \(ArkForgeFraming.maxFrameBytes)-byte limit"
    }
  }
}

/// One field as it came off the wire.
enum ProtobufValue {
  case varint(UInt64)
  case payload([UInt8])
  case fixed64(UInt64)
  case fixed32(UInt32)

  func asUInt64() throws -> UInt64 {
    guard case .varint(let value) = self else { throw ProtobufWireError.truncated }
    return value
  }

  func asBool() throws -> Bool {
    try asUInt64() != 0
  }

  func asInt32() throws -> Int32 {
    Int32(truncatingIfNeeded: try asUInt64())
  }

  func asBytes() throws -> [UInt8] {
    guard case .payload(let value) = self else { throw ProtobufWireError.truncated }
    return value
  }

  func asString(field: UInt32) throws -> String {
    let raw = try asBytes()
    guard let text = String(bytes: raw, encoding: .utf8) else {
      throw ProtobufWireError.invalidUTF8(field: field)
    }
    return text
  }
}

/// Reads proto3 fields, skipping the ones this build does not know.
struct ProtobufReader {
  private let source: [UInt8]
  private var cursor: Int
  private let depth: Int

  init(_ source: [UInt8], depth: Int = 0) {
    self.source = source
    self.cursor = 0
    self.depth = depth
  }

  init(_ source: Data, depth: Int = 0) {
    self.init(Array(source), depth: depth)
  }

  /// A hostile message must not be able to recurse the decoder to death.
  static let maxDepth = 16

  mutating func next() throws -> (field: UInt32, value: ProtobufValue)? {
    guard cursor < source.count else { return nil }
    let tag = try varint()
    let field = UInt32(tag >> 3)
    guard let type = ProtobufWireType(rawValue: tag & 0x07) else {
      throw ProtobufWireError.unknownWireType(tag & 0x07)
    }
    switch type {
    case .varint:
      return (field, .varint(try varint()))
    case .lengthDelimited:
      let length = Int(try varint())
      guard length >= 0, cursor + length <= source.count else {
        throw ProtobufWireError.truncated
      }
      let slice = Array(source[cursor..<(cursor + length)])
      cursor += length
      return (field, .payload(slice))
    case .fixed64:
      guard cursor + 8 <= source.count else { throw ProtobufWireError.truncated }
      var value: UInt64 = 0
      for offset in (0..<8).reversed() {
        value = value << 8 | UInt64(source[cursor + offset])
      }
      cursor += 8
      return (field, .fixed64(value))
    case .fixed32:
      guard cursor + 4 <= source.count else { throw ProtobufWireError.truncated }
      var value: UInt32 = 0
      for offset in (0..<4).reversed() {
        value = value << 8 | UInt32(source[cursor + offset])
      }
      cursor += 4
      return (field, .fixed32(value))
    }
  }

  /// A nested message reader, one level deeper.
  func nested(_ body: [UInt8]) throws -> ProtobufReader {
    guard depth + 1 < Self.maxDepth else { throw ProtobufWireError.tooDeep }
    return ProtobufReader(body, depth: depth + 1)
  }

  private mutating func varint() throws -> UInt64 {
    var value: UInt64 = 0
    var shift: UInt64 = 0
    while true {
      guard cursor < source.count else { throw ProtobufWireError.truncated }
      let byte = source[cursor]
      cursor += 1
      value |= UInt64(byte & 0x7f) << shift
      if byte & 0x80 == 0 { return value }
      shift += 7
      guard shift < 64 else { throw ProtobufWireError.malformedVarint }
    }
  }
}

/// Decodes an enum, refusing a value this build does not know.
func decodeEnum<Value>(
  _ message: String, _ field: UInt32, _ value: ProtobufValue,
  _ transform: (Int32) -> Value?
) throws -> Value {
  let raw = try value.asInt32()
  guard let decoded = transform(raw) else {
    throw ProtobufWireError.unknownEnumValue(
      message: message, field: field, value: Int64(raw))
  }
  return decoded
}
