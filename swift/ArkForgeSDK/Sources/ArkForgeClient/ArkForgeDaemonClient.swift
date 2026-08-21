import ArkForgeProtocol
import Darwin
import Foundation

public enum ArkForgeClientError: Error, CustomStringConvertible {
  case socketPathTooLong(path: String, limit: Int)
  case connectFailed(String)
  case transport(String)
  case handshakeRefused(String)
  case protocolMismatch(peerMajor: UInt32, peerMinor: UInt32)
  case wire(ProtobufWireError)
  case daemonRefused(api: ArkForgeApi, status: ArkForgeStatus, error: ArkForgeError?)

  public var description: String {
    switch self {
    case .socketPathTooLong(let path, let limit):
      return "socket path is \(path.utf8.count) bytes but the platform limit is \(limit)"
    case .connectFailed(let detail): return "connect failed: \(detail)"
    case .transport(let detail): return detail
    case .handshakeRefused(let reason): return "daemon refused the session: \(reason)"
    case .protocolMismatch(let major, let minor):
      return
        "daemon speaks protocol \(major).\(minor); this build speaks "
        + "\(ArkForgeHello.protocolMajor).\(ArkForgeHello.protocolMinor)"
    case .wire(let error): return "wire: \(error)"
    case .daemonRefused(let api, let status, let error):
      let detail = error.map { " (\($0))" } ?? ""
      return "\(api) refused with \(status)\(detail)"
    }
  }
}

/// A controller-session client for `arkforged`.
///
/// # What this owns and does not own
///
/// It carries bytes. Every decision about *whether* a step may run stays with
/// the caller: this type will encode a permit the caller signed and will
/// encode a refusal the caller chose, and it has no way to construct either
/// one for itself. That is the same split the daemon enforces from its side —
/// `arkforged` verifies permits and cannot mint them.
///
/// # Why the daemon never calls out
///
/// Every message is client-initiated. The daemon *asks* on the `watchJob`
/// stream and waits for the authority to call back in on a second request,
/// which leaves the authority free to answer, to refuse, or to say nothing —
/// three outcomes the daemon distinguishes (design §3.1). So this client
/// exposes a stream you pull from, not a delegate the daemon pushes to.
/// `@unchecked Sendable` with the lock that earns it.
///
/// This owns a socket and a read buffer, neither of which may be used from two
/// places at once: two interleaved `call`s would each read a frame the other
/// was waiting for. The lock below serializes the whole request surface, which
/// is what makes it safe to hand to an actor — the alternative, declaring it
/// Sendable and hoping, is the bug this prevents rather than documents.
package final class ArkForgeLocalConnection: @unchecked Sendable {
  private let descriptor: Int32
  private let exchange = NSLock()
  private var pending: [UInt8] = []
  public let helloAck: ArkForgeHelloAck

  /// Opens a controller session and completes the handshake.
  ///
  /// The handshake is not a formality: it carries the two standing execution
  /// facts and the bound toolchain digest, and a caller that ignores them can
  /// materialize a plan this daemon could never run.
  public init(
    socketPath: String, sessionKind: ArkForgeSessionKind = .controller,
    timeoutSeconds: Int = 30
  ) throws {
    let fd = socket(AF_UNIX, SOCK_STREAM, 0)
    guard fd >= 0 else {
      throw ArkForgeClientError.connectFailed("socket() failed: errno \(errno)")
    }
    var opened = true
    defer { if !opened { close(fd) } }
    opened = false

    var suppressSignal: Int32 = 1
    guard
      setsockopt(
        fd, SOL_SOCKET, SO_NOSIGPIPE, &suppressSignal, socklen_t(MemoryLayout<Int32>.size)) == 0
    else { throw ArkForgeClientError.transport("cannot suppress SIGPIPE") }

    // A write that never returns would hang whichever queue drives it. Every
    // call here is bounded; a partition write takes minutes, but it reports
    // progress as events rather than by blocking one read for its duration.
    var timeout = timeval(tv_sec: timeoutSeconds, tv_usec: 0)
    guard
      setsockopt(
        fd, SOL_SOCKET, SO_SNDTIMEO, &timeout, socklen_t(MemoryLayout.size(ofValue: timeout)))
        == 0,
      setsockopt(
        fd, SOL_SOCKET, SO_RCVTIMEO, &timeout, socklen_t(MemoryLayout.size(ofValue: timeout)))
        == 0
    else { throw ArkForgeClientError.transport("cannot configure bounded socket timeout") }

    var address = sockaddr_un()
    address.sun_family = sa_family_t(AF_UNIX)
    let limit = MemoryLayout.size(ofValue: address.sun_path)
    guard socketPath.utf8.count < limit else {
      throw ArkForgeClientError.socketPathTooLong(path: socketPath, limit: limit - 1)
    }
    withUnsafeMutableBytes(of: &address.sun_path) { buffer in
      socketPath.utf8CString.withUnsafeBytes { source in
        buffer.copyMemory(from: UnsafeRawBufferPointer(rebasing: source.prefix(buffer.count)))
      }
    }
    let connected = withUnsafePointer(to: &address) { pointer in
      pointer.withMemoryRebound(to: sockaddr.self, capacity: 1) { sockaddrPointer in
        Darwin.connect(fd, sockaddrPointer, socklen_t(MemoryLayout<sockaddr_un>.size))
      }
    }
    guard connected == 0 else {
      throw ArkForgeClientError.connectFailed("errno \(errno) for \(socketPath)")
    }
    opened = true
    self.descriptor = fd

    try Self.writeFrame(fd, ArkForgeHello(sessionKind: sessionKind).encoded)
    var buffer: [UInt8] = []
    guard let ackFrame = try Self.readFrame(fd, pending: &buffer) else {
      throw ArkForgeClientError.transport("daemon closed before acknowledging the handshake")
    }
    self.pending = buffer
    let ack: ArkForgeHelloAck
    do {
      ack = try ArkForgeHelloAck.decode(ackFrame)
    } catch let error as ProtobufWireError {
      throw ArkForgeClientError.wire(error)
    }
    if let refusal = ack.refusal {
      throw ArkForgeClientError.handshakeRefused(refusal)
    }
    guard ack.protocolMajor == ArkForgeHello.protocolMajor else {
      throw ArkForgeClientError.protocolMismatch(
        peerMajor: ack.protocolMajor, peerMinor: ack.protocolMinor)
    }
    self.helloAck = ack
  }

  deinit { close(descriptor) }

  public func closeSession() {
    close(descriptor)
  }

  // MARK: - Calls

  /// Sends a request and reads exactly one response.
  public func call(_ request: ArkForgeRequest) throws -> ArkForgeResponse {
    exchange.lock()
    defer { exchange.unlock() }
    try Self.writeFrame(descriptor, request.encoded)
    guard let frame = try Self.readFrame(descriptor, pending: &pending) else {
      throw ArkForgeClientError.transport("daemon closed while answering \(request.api)")
    }
    do {
      return try ArkForgeResponse.decode(frame)
    } catch let error as ProtobufWireError {
      throw ArkForgeClientError.wire(error)
    }
  }

  /// Sends a request and returns the payload only when the daemon answered OK.
  ///
  /// A refusal is raised rather than returned, because every caller of these
  /// APIs treats "refused" as a stop: `startExecution` refused means no job
  /// exists, and continuing as though one did is how a caller ends up waiting
  /// for events that will never arrive.
  public func callExpectingOK(_ request: ArkForgeRequest) throws -> Data {
    let response = try call(request)
    guard response.status == .ok else {
      throw ArkForgeClientError.daemonRefused(
        api: request.api, status: response.status, error: try response.errorBody())
    }
    return response.payload
  }

  public func startExecution(_ body: ArkForgeStartExecutionRequest, requestID: String) throws
    -> ArkForgeStartExecutionResponse
  {
    let payload = try callExpectingOK(
      ArkForgeRequest(requestID: requestID, api: .startExecution, payload: body.encoded))
    return try ArkForgeStartExecutionResponse.decode(payload)
  }

  public func submitStepPermit(_ body: ArkForgeSubmitStepPermitRequest, requestID: String) throws
    -> ArkForgeSubmitStepPermitResponse
  {
    let payload = try callExpectingOK(
      ArkForgeRequest(requestID: requestID, api: .submitStepPermit, payload: body.encoded))
    return try ArkForgeSubmitStepPermitResponse.decode(payload)
  }

  public func submitManagedControlReceipt(
    _ body: ArkForgeSubmitManagedControlReceiptRequest, requestID: String
  ) throws -> ArkForgeSubmitManagedControlReceiptResponse {
    let payload = try callExpectingOK(
      ArkForgeRequest(
        requestID: requestID, api: .submitManagedControlReceipt, payload: body.encoded))
    return try ArkForgeSubmitManagedControlReceiptResponse.decode(payload)
  }

  /// Requests cancellation.
  ///
  /// Returns the cancellation state when the daemon could settle one. A
  /// `CANCEL_NOT_SAFE` refusal surfaces as `daemonRefused`, and per design
  /// §6.3.1 that is a *refused* cancellation — the write is still running and
  /// will produce its own receipt. It must not be recorded as an unconfirmed
  /// teardown; there is no process group here for this authority to tear down.
  public func cancelJob(jobID: String, requestID: String) throws -> ArkForgeCancelJobResponse {
    let payload = try callExpectingOK(
      ArkForgeRequest(
        requestID: requestID, api: .cancelJob,
        payload: ArkForgeCancelJobRequest(jobID: jobID).encoded))
    return try ArkForgeCancelJobResponse.decode(payload)
  }

  /// Point-in-time durable status. Unlike `watchJob`, callers need no event
  /// cursor, and the daemon reconstructs this after restart.
  public func getJob(jobID: String, requestID: String) throws -> ArkForgeJobSummary {
    let payload = try callExpectingOK(
      ArkForgeRequest(
        requestID: requestID, api: .getJob,
        payload: ArkForgeGetJobRequest(jobID: jobID).encoded))
    return try ArkForgeJobSummary.decodeResponse(payload)
  }

  /// Every known ArkForge job with bounded progress fields and no timelines.
  public func listJobs(requestID: String) throws -> [ArkForgeJobSummary] {
    let payload = try callExpectingOK(
      ArkForgeRequest(requestID: requestID, api: .listJobs, payload: Data()))
    return try ArkForgeJobSummary.decodeList(payload)
  }

  // MARK: - Putting a plan in the daemon's store

  /// Streams an archive into the daemon's content-addressed store.
  ///
  /// The header states size and digest before a byte moves, so an import that
  /// cannot succeed is refused rather than half-written. Content follows as
  /// frames on this same connection, terminated by an empty frame — the shape
  /// the daemon's `ContentStream` reads.
  ///
  /// Read in chunks rather than loaded whole: a DAYU200 daily is ~731 MB, and
  /// a client that had to hold one in memory to send it would fail first on
  /// the machine with the least room.
  ///
  /// Importing the same bytes twice is not an error. The store is addressed by
  /// content, so a second import returns the same `artifactID` and says
  /// `deduplicated`.
  public func importArtifact(
    contentsOf url: URL, expectedSHA256: String, requestID: String
  ) throws -> ArkForgeImportArtifactResponse {
    let handle = try FileHandle(forReadingFrom: url)
    defer { try? handle.close() }
    let size = (try FileManager.default.attributesOfItem(atPath: url.path)[.size] as? UInt64) ?? 0

    exchange.lock()
    defer { exchange.unlock() }
    try Self.writeFrame(
      descriptor,
      ArkForgeRequest(
        requestID: requestID, api: .importArtifact,
        payload: ArkForgeImportArtifactRequest(
          expectedSizeBytes: size, expectedSHA256: expectedSHA256
        ).encoded
      ).encoded)

    while true {
      let chunk = try handle.read(upToCount: Self.importChunkBytes) ?? Data()
      if chunk.isEmpty { break }
      try Self.writeFrame(descriptor, chunk)
    }
    // The empty frame is the terminator, not a courtesy: without it the daemon
    // waits for content it was told to expect.
    try Self.writeFrame(descriptor, Data())

    guard let frame = try Self.readFrame(descriptor, pending: &pending) else {
      throw ArkForgeClientError.transport("daemon closed while taking the artifact")
    }
    let response = try ArkForgeResponse.decode(frame)
    guard response.status == .ok else {
      throw ArkForgeClientError.daemonRefused(
        api: .importArtifact, status: response.status, error: try response.errorBody())
    }
    return try ArkForgeImportArtifactResponse.decode(response.payload)
  }

  /// 4 MiB: a 731 MB archive becomes ~180 frames rather than tens of
  /// thousands, without holding the archive in memory.
  static let importChunkBytes = 4 * 1024 * 1024

  /// The bound a materialization connection needs.
  ///
  /// Every call in that phase is proportional to the archive rather than to a
  /// message: `importArtifact` hashes and stores ~731 MB before answering, and
  /// `inspectArtifact` decompresses and walks the same archive to build a
  /// manifest. Measured 2026-08-17: both exceed the 30 s default on a DAYU200
  /// daily, and the second one is why raising it for import alone was not
  /// enough.
  ///
  /// Fifteen minutes is still a bound, and still guards the thing worth
  /// guarding — a daemon that has stopped answering. It is not a throughput
  /// estimate, and nothing here should be read as one.
  public static let materializationTimeoutSeconds = 900

  /// Builds the daemon's manifest for an imported artifact.
  ///
  /// Required before `materializePlan`, which refuses with
  /// `ARTIFACT_NOT_INSPECTED` until it has run: the manifest is what the
  /// provider validates the profile against.
  public func inspectArtifact(artifactID: String, requestID: String) throws
    -> ArkForgeInspectArtifactResponse
  {
    let payload = try callExpectingOK(
      ArkForgeRequest(
        requestID: requestID, api: .inspectArtifact,
        payload: ArkForgeInspectArtifactRequest(artifactID: artifactID).encoded))
    return try ArkForgeInspectArtifactResponse.decode(payload)
  }

  /// Every device the daemon can see through its own transports.
  ///
  /// Its USB enumeration is read-only ioreg: it opens no HDC server and takes
  /// no device, so this observes the same board ArkDeck holds rather than
  /// competing for it.
  public func discoverDevices(requestID: String) throws -> [ArkForgeDeviceObservation] {
    let payload = try callExpectingOK(
      ArkForgeRequest(requestID: requestID, api: .discoverDevices, payload: Data()))
    return try ArkForgeDeviceObservation.decodeList(payload)
  }

  /// Materializes a plan for one artifact, profile and observed device.
  ///
  /// The answer is a plan **or** an assessment, and an assessment is not a
  /// failure: it is the daemon reporting that it built the whole plan and
  /// declined to make it executable. Both must be handled — treating an
  /// assessment as an error discards the reasons, which are the only part an
  /// operator can act on.
  public func materializePlan(
    _ body: ArkForgeMaterializePlanRequest, requestID: String
  ) throws -> ArkForgeMaterializePlanResponse {
    let payload = try callExpectingOK(
      ArkForgeRequest(requestID: requestID, api: .materializePlan, payload: body.encoded))
    return try ArkForgeMaterializePlanResponse.decode(payload)
  }

  /// Streams job events, calling `handle` for each until the stream ends.
  ///
  /// `handle` returns `false` to stop reading early. The daemon polls rather
  /// than pushes (design §3.2), so a handler that blocks holds up nothing on
  /// the daemon side except this one stream.
  ///
  /// Sequence numbers are the journal's, so a gap means this authority missed
  /// an event — not that none happened. Reconnect with `fromSequence` set to
  /// the last one seen.
  public func watchJob(
    _ body: ArkForgeWatchJobRequest, requestID: String,
    handle: (ArkForgeJobEvent) throws -> Bool
  ) throws {
    exchange.lock()
    defer { exchange.unlock() }
    try Self.writeFrame(
      descriptor,
      ArkForgeRequest(requestID: requestID, api: .watchJob, payload: body.encoded).encoded)
    while true {
      guard let frame = try Self.readFrame(descriptor, pending: &pending) else { return }
      let response: ArkForgeResponse
      do {
        response = try ArkForgeResponse.decode(frame)
      } catch let error as ProtobufWireError {
        throw ArkForgeClientError.wire(error)
      }
      guard response.status == .ok else {
        throw ArkForgeClientError.daemonRefused(
          api: .watchJob, status: response.status, error: try response.errorBody())
      }
      if !response.payload.isEmpty {
        // `arkforged` frames this payload as a *repeated* JobEvent at field 1,
        // so one frame can carry several events — it writes one nested message
        // per event it has queued. Decoding the payload as a single bare event
        // read that wrapper's first nested message as `jobId` and failed on the
        // first byte of it that is not UTF-8, which is what a length prefix
        // usually is. The stream is therefore unreadable the moment arkforged
        // has anything to say, which is every real flash.
        do {
          for event in try ArkForgeJobEvent.decodeList(response.payload) {
            if try !handle(event) { return }
          }
        } catch let error as ProtobufWireError {
          throw ArkForgeClientError.wire(error)
        }
      }
      if response.streamEnd { return }
    }
  }

  // MARK: - Frames

  static func writeFrame(_ fd: Int32, _ body: Data) throws {
    guard body.count <= ArkForgeFraming.maxFrameBytes else {
      throw ArkForgeClientError.wire(.frameTooLarge(body.count))
    }
    var payload = Data()
    let length = UInt32(body.count)
    payload.append(contentsOf: [
      UInt8(truncatingIfNeeded: length >> 24), UInt8(truncatingIfNeeded: length >> 16),
      UInt8(truncatingIfNeeded: length >> 8), UInt8(truncatingIfNeeded: length),
    ])
    payload.append(body)

    var written = 0
    let ok: Bool = payload.withUnsafeBytes { raw in
      guard let base = raw.baseAddress else { return false }
      while written < payload.count {
        let result = write(fd, base + written, payload.count - written)
        if result <= 0 { return false }
        written += result
      }
      return true
    }
    guard ok else { throw ArkForgeClientError.transport("short write after \(written) bytes") }
  }

  /// Reads one frame, buffering whatever arrived past its end.
  ///
  /// Returns nil at a clean end of stream: the peer closing *between* frames
  /// is not an error, and treating it as one would turn every normal
  /// disconnection into a failure to report.
  static func readFrame(_ fd: Int32, pending: inout [UInt8]) throws -> Data? {
    while pending.count < 4 {
      guard try fill(fd, into: &pending) else {
        if pending.isEmpty { return nil }
        throw ArkForgeClientError.transport("stream ended inside a frame header")
      }
    }
    let length =
      Int(pending[0]) << 24 | Int(pending[1]) << 16 | Int(pending[2]) << 8 | Int(pending[3])
    guard length <= ArkForgeFraming.maxFrameBytes else {
      throw ArkForgeClientError.wire(.frameTooLarge(length))
    }
    while pending.count < 4 + length {
      guard try fill(fd, into: &pending) else {
        throw ArkForgeClientError.transport("stream ended inside a frame body")
      }
    }
    let body = Data(pending[4..<(4 + length)])
    pending.removeFirst(4 + length)
    return body
  }

  private static func fill(_ fd: Int32, into buffer: inout [UInt8]) throws -> Bool {
    var chunk = [UInt8](repeating: 0, count: 64 * 1024)
    let count = read(fd, &chunk, chunk.count)
    if count == 0 { return false }
    if count < 0 {
      if errno == EAGAIN || errno == EWOULDBLOCK {
        throw ArkForgeClientError.transport("timed out waiting for the daemon")
      }
      throw ArkForgeClientError.transport("read failed: errno \(errno)")
    }
    buffer.append(contentsOf: chunk[0..<count])
    return true
  }
}
