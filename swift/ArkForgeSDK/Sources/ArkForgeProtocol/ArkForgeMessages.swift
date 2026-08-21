import Foundation

/// The message subset of `proto/arkforge.proto` this authority needs.
///
/// Field numbers are the contract and are written out literally next to each
/// access so a reader can check them against the `.proto` without holding a
/// mapping in their head. Numbers are never reused; a field this build does
/// not know is skipped, and an enum value it does not know is refused.
///
/// Not every message in the schema is here. The read-only half (`inspect`,
/// `discover`, `probe`, `materialize`) is reachable through the same envelope
/// when it is needed; what this file covers is the execution surface the
/// permit route runs on — APIs 6, 7, 8, 12 and 13.
public enum ArkForgeSessionKind: Int32, Sendable {
  case unspecified = 0
  case publicSession = 1
  case controller = 2
}

public enum ArkForgeApi: Int32, Sendable {
  case unspecified = 0
  case importArtifact = 1
  case inspectArtifact = 2
  case discoverDevices = 3
  case probeDevice = 4
  case materializePlan = 5
  case startExecution = 6
  case watchJob = 7
  case cancelJob = 8
  case reconcileJob = 9
  case planSupersedingRecovery = 10
  case getRecoveryGuide = 11
  case submitStepPermit = 12
  case submitManagedControlReceipt = 13
  case getJob = 14
  case listJobs = 15
}

public enum ArkForgeStatus: Int32, Sendable {
  case unspecified = 0
  case ok = 1
  case refused = 2
  case unavailable = 3
  case invalidArgument = 4
  case notFound = 5
  case internalFailure = 6
}

public enum ArkForgeJobEventKind: Int32, Sendable {
  case unspecified = 0
  case stateChanged = 1
  case stepAdmissionRequested = 2
  case managedControlRequested = 3
  case actionReceipt = 4
  case stepCheckpointed = 5
  case postflightRecorded = 6
  case outcomeClassified = 7
  case possibleEffectSet = 8
  case recoveryAssessment = 9
}

public enum ArkForgeManagedControlAction: Int32, Sendable {
  case unspecified = 0
  case enterUpdater = 1
  case rebootToNormal = 2
  case readProductFacts = 3
  case readBuildFacts = 4
}

public struct ArkForgeKeyValue: Sendable, Equatable {
  public let key: String
  public let value: String

  public init(key: String, value: String) {
    self.key = key
    self.value = value
  }

  var encoded: [UInt8] {
    var writer = ProtobufWriter()
    writer.string(1, key)
    writer.string(2, value)
    return writer.bytes
  }

  static func decode(_ body: [UInt8], within reader: ProtobufReader) throws -> ArkForgeKeyValue {
    var nested = try reader.nested(body)
    var key = ""
    var value = ""
    while let field = try nested.next() {
      switch field.field {
      case 1: key = try field.value.asString(field: 1)
      case 2: value = try field.value.asString(field: 2)
      default: break
      }
    }
    return ArkForgeKeyValue(key: key, value: value)
  }
}

// MARK: - Session

public struct ArkForgeHello: Sendable {
  public static let protocolMajor: UInt32 = 1
  public static let protocolMinor: UInt32 = 0

  public let sessionKind: ArkForgeSessionKind

  public init(sessionKind: ArkForgeSessionKind) {
    self.sessionKind = sessionKind
  }

  public var encoded: Data {
    var writer = ProtobufWriter()
    writer.uint32(1, Self.protocolMajor)
    writer.uint32(2, Self.protocolMinor)
    writer.enumeration(3, sessionKind.rawValue)
    return writer.data
  }
}

/// The daemon's answer, including the two standing execution facts.
///
/// `executionReady` is empty-blockers, not a separate opinion: a client that
/// reads only one of them can materialize a plan it cannot run. And a bound
/// toolchain digest here that differs from the plan's is a combination nobody
/// published — the daemon refuses it at `startExecution`, and this field is so
/// a client need not find out by being refused.
public struct ArkForgeHelloAck: Sendable, Equatable {
  public let protocolMajor: UInt32
  public let protocolMinor: UInt32
  public let sessionKind: ArkForgeSessionKind
  public let daemonVersion: String
  public let refusal: String?
  public let executionReady: Bool
  public let executionBlockers: [String]
  public let toolchainID: String
  public let toolchainSHA256: String

  public static func decode(_ body: Data) throws -> ArkForgeHelloAck {
    var reader = ProtobufReader(body)
    var major: UInt32 = 0
    var minor: UInt32 = 0
    var kind: ArkForgeSessionKind = .unspecified
    var version = ""
    var refusal = ""
    var ready = false
    var blockers: [String] = []
    var toolchainID = ""
    var toolchainSHA = ""
    while let field = try reader.next() {
      switch field.field {
      case 1: major = UInt32(truncatingIfNeeded: try field.value.asUInt64())
      case 2: minor = UInt32(truncatingIfNeeded: try field.value.asUInt64())
      case 3:
        kind = try decodeEnum("HelloAck", 3, field.value, ArkForgeSessionKind.init(rawValue:))
      case 4: version = try field.value.asString(field: 4)
      case 5: refusal = try field.value.asString(field: 5)
      case 6: ready = try field.value.asBool()
      case 7: blockers.append(try field.value.asString(field: 7))
      case 8: toolchainID = try field.value.asString(field: 8)
      case 9: toolchainSHA = try field.value.asString(field: 9)
      default: break
      }
    }
    return ArkForgeHelloAck(
      protocolMajor: major, protocolMinor: minor, sessionKind: kind, daemonVersion: version,
      refusal: refusal.isEmpty ? nil : refusal, executionReady: ready,
      executionBlockers: blockers, toolchainID: toolchainID, toolchainSHA256: toolchainSHA)
  }
}

// MARK: - Envelope

public struct ArkForgeRequest: Sendable {
  public let requestID: String
  public let api: ArkForgeApi
  public let payload: Data

  public init(requestID: String, api: ArkForgeApi, payload: Data) {
    self.requestID = requestID
    self.api = api
    self.payload = payload
  }

  public var encoded: Data {
    var writer = ProtobufWriter()
    writer.string(1, requestID)
    writer.enumeration(2, api.rawValue)
    writer.bytes(3, payload)
    return writer.data
  }
}

public struct ArkForgeResponse: Sendable, Equatable {
  public let requestID: String
  public let api: ArkForgeApi
  public let status: ArkForgeStatus
  public let payload: Data
  public let streamSequence: UInt64
  public let streamEnd: Bool

  public static func decode(_ body: Data) throws -> ArkForgeResponse {
    var reader = ProtobufReader(body)
    var requestID = ""
    var api: ArkForgeApi = .unspecified
    var status: ArkForgeStatus = .unspecified
    var payload = Data()
    var sequence: UInt64 = 0
    var end = false
    while let field = try reader.next() {
      switch field.field {
      case 1: requestID = try field.value.asString(field: 1)
      case 2: api = try decodeEnum("Response", 2, field.value, ArkForgeApi.init(rawValue:))
      case 3: status = try decodeEnum("Response", 3, field.value, ArkForgeStatus.init(rawValue:))
      case 4: payload = Data(try field.value.asBytes())
      case 5: sequence = try field.value.asUInt64()
      case 6: end = try field.value.asBool()
      default: break
      }
    }
    return ArkForgeResponse(
      requestID: requestID, api: api, status: status, payload: payload,
      streamSequence: sequence, streamEnd: end)
  }

  /// The daemon's typed error body, present on every non-OK status.
  public func errorBody() throws -> ArkForgeError? {
    guard status != .ok, !payload.isEmpty else { return nil }
    return try ArkForgeError.decode(payload)
  }
}

public struct ArkForgeError: Sendable, Equatable, CustomStringConvertible {
  public let code: String
  public let message: String

  public var description: String { "\(code): \(message)" }

  static func decode(_ body: Data) throws -> ArkForgeError {
    var reader = ProtobufReader(body)
    var code = ""
    var message = ""
    while let field = try reader.next() {
      switch field.field {
      case 1: code = try field.value.asString(field: 1)
      case 2: message = try field.value.asString(field: 2)
      default: break
      }
    }
    return ArkForgeError(code: code, message: message)
  }
}

// MARK: - Execution

public struct ArkForgeStartExecutionRequest: Sendable {
  public let planID: String
  public let planSHA256: String
  public let executionPurpose: String
  public let controllerSessionID: String

  public init(
    planID: String, planSHA256: String, executionPurpose: String, controllerSessionID: String
  ) {
    self.planID = planID
    self.planSHA256 = planSHA256
    self.executionPurpose = executionPurpose
    self.controllerSessionID = controllerSessionID
  }

  public var encoded: Data {
    var writer = ProtobufWriter()
    writer.string(1, planID)
    writer.string(2, planSHA256)
    writer.string(3, executionPurpose)
    writer.string(4, controllerSessionID)
    return writer.data
  }
}

public struct ArkForgeStartExecutionResponse: Sendable, Equatable {
  public let jobID: String

  public static func decode(_ body: Data) throws -> ArkForgeStartExecutionResponse {
    var reader = ProtobufReader(body)
    var jobID = ""
    while let field = try reader.next() {
      if field.field == 1 { jobID = try field.value.asString(field: 1) }
    }
    return ArkForgeStartExecutionResponse(jobID: jobID)
  }
}

public struct ArkForgeWatchJobRequest: Sendable {
  public let jobID: String
  /// 0 means "from the beginning". A gap in the sequence means the authority
  /// missed an event, not that none happened.
  public let fromSequence: UInt64

  public init(jobID: String, fromSequence: UInt64 = 0) {
    self.jobID = jobID
    self.fromSequence = fromSequence
  }

  public var encoded: Data {
    var writer = ProtobufWriter()
    writer.string(1, jobID)
    writer.uint64(2, fromSequence)
    return writer.data
  }
}

/// What the daemon read immediately before asking for a permit.
///
/// The authority re-verifies every field against its own binding before it
/// signs. A snapshot echoed back has proved nothing — see the design's §3.3.
public struct ArkForgeStepAdmissionSnapshot: Sendable, Equatable {
  public let jobID: String
  public let planID: String
  public let planSHA256: [UInt8]
  public let stepID: String
  public let attemptID: String
  public let publicStepSHA256: [UInt8]
  public let privateActionSHA256: [UInt8]
  public let effectSetSHA256: [UInt8]
  public let admittedDeviceFactsSHA256: [UInt8]
  public let observedMode: String
  public let observedAtEpochMs: UInt64
  public let snapshotLifetimeMs: UInt64
  public let requestID: String
  public let topologySHA256: [UInt8]
  public let descriptorSHA256: [UInt8]
  public let serialSHA256: [UInt8]
  public let serialEvidenceKind: String
  public let protocolIdentity: [ArkForgeKeyValue]
  public let identityStrength: String
  public let malformedDescriptor: Bool
  public let transportSessionSHA256: [UInt8]

  public init(
    jobID: String, planID: String, planSHA256: [UInt8], stepID: String,
    attemptID: String, publicStepSHA256: [UInt8], privateActionSHA256: [UInt8],
    effectSetSHA256: [UInt8], admittedDeviceFactsSHA256: [UInt8], observedMode: String,
    observedAtEpochMs: UInt64, snapshotLifetimeMs: UInt64, requestID: String,
    topologySHA256: [UInt8] = [], descriptorSHA256: [UInt8] = [],
    serialSHA256: [UInt8] = [], serialEvidenceKind: String = "",
    protocolIdentity: [ArkForgeKeyValue] = [], identityStrength: String = "",
    malformedDescriptor: Bool = false, transportSessionSHA256: [UInt8] = []
  ) {
    self.jobID = jobID
    self.planID = planID
    self.planSHA256 = planSHA256
    self.stepID = stepID
    self.attemptID = attemptID
    self.publicStepSHA256 = publicStepSHA256
    self.privateActionSHA256 = privateActionSHA256
    self.effectSetSHA256 = effectSetSHA256
    self.admittedDeviceFactsSHA256 = admittedDeviceFactsSHA256
    self.observedMode = observedMode
    self.observedAtEpochMs = observedAtEpochMs
    self.snapshotLifetimeMs = snapshotLifetimeMs
    self.requestID = requestID
    self.topologySHA256 = topologySHA256
    self.descriptorSHA256 = descriptorSHA256
    self.serialSHA256 = serialSHA256
    self.serialEvidenceKind = serialEvidenceKind
    self.protocolIdentity = protocolIdentity
    self.identityStrength = identityStrength
    self.malformedDescriptor = malformedDescriptor
    self.transportSessionSHA256 = transportSessionSHA256
  }

  /// Whether this snapshot may still be signed against at `now`.
  ///
  /// Past it the daemon takes a new snapshot rather than accepting a late
  /// permit, so signing an expired one wastes an admission round-trip and
  /// tells the daemon the authority is not checking.
  public func isFresh(atEpochMs now: UInt64) -> Bool {
    now >= observedAtEpochMs && now - observedAtEpochMs <= snapshotLifetimeMs
  }

  static func decode(_ body: [UInt8], within reader: ProtobufReader) throws
    -> ArkForgeStepAdmissionSnapshot
  {
    var nested = try reader.nested(body)
    var jobID = ""
    var planID = ""
    var stepID = ""
    var attemptID = ""
    var mode = ""
    var requestID = ""
    var planDigest: [UInt8] = []
    var publicStep: [UInt8] = []
    var privateAction: [UInt8] = []
    var effectSet: [UInt8] = []
    var deviceFacts: [UInt8] = []
    var topology: [UInt8] = []
    var descriptor: [UInt8] = []
    var serial: [UInt8] = []
    var serialKind = ""
    var identityStrength = ""
    var protocolIdentity: [ArkForgeKeyValue] = []
    var malformedDescriptor = false
    var transportSession: [UInt8] = []
    var observedAt: UInt64 = 0
    var lifetime: UInt64 = 0
    while let field = try nested.next() {
      switch field.field {
      case 1: jobID = try field.value.asString(field: 1)
      case 2: planID = try field.value.asString(field: 2)
      case 3: planDigest = try field.value.asBytes()
      case 4: stepID = try field.value.asString(field: 4)
      case 5: attemptID = try field.value.asString(field: 5)
      case 6: publicStep = try field.value.asBytes()
      case 7: privateAction = try field.value.asBytes()
      case 8: effectSet = try field.value.asBytes()
      case 9: deviceFacts = try field.value.asBytes()
      case 10: mode = try field.value.asString(field: 10)
      case 11: observedAt = try field.value.asUInt64()
      case 12: lifetime = try field.value.asUInt64()
      case 13: requestID = try field.value.asString(field: 13)
      case 14: topology = try field.value.asBytes()
      case 15: descriptor = try field.value.asBytes()
      case 16: serial = try field.value.asBytes()
      case 17: serialKind = try field.value.asString(field: 17)
      case 18:
        protocolIdentity.append(
          try ArkForgeKeyValue.decode(try field.value.asBytes(), within: nested))
      case 19: identityStrength = try field.value.asString(field: 19)
      case 20: malformedDescriptor = try field.value.asBool()
      case 21: transportSession = try field.value.asBytes()
      default: break
      }
    }
    return ArkForgeStepAdmissionSnapshot(
      jobID: jobID, planID: planID, planSHA256: planDigest, stepID: stepID,
      attemptID: attemptID, publicStepSHA256: publicStep, privateActionSHA256: privateAction,
      effectSetSHA256: effectSet, admittedDeviceFactsSHA256: deviceFacts, observedMode: mode,
      observedAtEpochMs: observedAt, snapshotLifetimeMs: lifetime, requestID: requestID,
      topologySHA256: topology, descriptorSHA256: descriptor, serialSHA256: serial,
      serialEvidenceKind: serialKind, protocolIdentity: protocolIdentity,
      identityStrength: identityStrength, malformedDescriptor: malformedDescriptor,
      transportSessionSHA256: transportSession)
  }
}

public struct ArkForgeManagedControlRequest: Sendable, Equatable {
  public let jobID: String
  public let stepID: String
  public let requestID: String
  public let action: ArkForgeManagedControlAction
  public let permitID: String
  public let expectedFacts: [ArkForgeKeyValue]
  public let deadlineEpochMs: UInt64

  static func decode(_ body: [UInt8], within reader: ProtobufReader) throws
    -> ArkForgeManagedControlRequest
  {
    var nested = try reader.nested(body)
    var jobID = ""
    var stepID = ""
    var requestID = ""
    var permitID = ""
    var action: ArkForgeManagedControlAction = .unspecified
    var facts: [ArkForgeKeyValue] = []
    var deadline: UInt64 = 0
    while let field = try nested.next() {
      switch field.field {
      case 1: jobID = try field.value.asString(field: 1)
      case 2: stepID = try field.value.asString(field: 2)
      case 3: requestID = try field.value.asString(field: 3)
      case 4:
        action = try decodeEnum(
          "ManagedControlRequest", 4, field.value,
          ArkForgeManagedControlAction.init(rawValue:))
      case 5: permitID = try field.value.asString(field: 5)
      case 6: facts.append(try ArkForgeKeyValue.decode(try field.value.asBytes(), within: nested))
      case 7: deadline = try field.value.asUInt64()
      default: break
      }
    }
    return ArkForgeManagedControlRequest(
      jobID: jobID, stepID: stepID, requestID: requestID, action: action, permitID: permitID,
      expectedFacts: facts, deadlineEpochMs: deadline)
  }
}

public struct ArkForgeActionReceiptSummary: Sendable, Equatable {
  public let jobID: String
  public let planID: String
  public let stepID: String
  public let actionID: String
  public let attemptID: String
  public let permitID: String
  /// `semanticSuccess | confirmedNoEffect | confirmedPartialEffect | outcomeUnknown`.
  /// A zero exit status is not a disposition and never appears here.
  public let disposition: String
  public let evidenceSHA256: [UInt8]
  public let verificationOutcome: String
  public let verificationStrength: String
  public let verifiedRangeStart: UInt64
  public let verifiedRangeLength: UInt64
  public let typedSkipReason: String
  public let failureClassification: String
  public let facts: [ArkForgeKeyValue]

  static func decode(_ body: [UInt8], within reader: ProtobufReader) throws
    -> ArkForgeActionReceiptSummary
  {
    var nested = try reader.nested(body)
    var jobID = ""
    var planID = ""
    var stepID = ""
    var actionID = ""
    var attemptID = ""
    var permitID = ""
    var disposition = ""
    var outcome = ""
    var strength = ""
    var skipReason = ""
    var failure = ""
    var evidence: [UInt8] = []
    var rangeStart: UInt64 = 0
    var rangeLength: UInt64 = 0
    var facts: [ArkForgeKeyValue] = []
    while let field = try nested.next() {
      switch field.field {
      case 1: jobID = try field.value.asString(field: 1)
      case 2: planID = try field.value.asString(field: 2)
      case 3: stepID = try field.value.asString(field: 3)
      case 4: actionID = try field.value.asString(field: 4)
      case 5: attemptID = try field.value.asString(field: 5)
      case 6: permitID = try field.value.asString(field: 6)
      case 7: disposition = try field.value.asString(field: 7)
      case 8: evidence = try field.value.asBytes()
      case 9: outcome = try field.value.asString(field: 9)
      case 10: strength = try field.value.asString(field: 10)
      case 11: rangeStart = try field.value.asUInt64()
      case 12: rangeLength = try field.value.asUInt64()
      case 13: skipReason = try field.value.asString(field: 13)
      case 14: failure = try field.value.asString(field: 14)
      case 15: facts.append(try ArkForgeKeyValue.decode(try field.value.asBytes(), within: nested))
      default: break
      }
    }
    return ArkForgeActionReceiptSummary(
      jobID: jobID, planID: planID, stepID: stepID, actionID: actionID, attemptID: attemptID,
      permitID: permitID, disposition: disposition, evidenceSHA256: evidence,
      verificationOutcome: outcome, verificationStrength: strength,
      verifiedRangeStart: rangeStart, verifiedRangeLength: rangeLength,
      typedSkipReason: skipReason, failureClassification: failure, facts: facts)
  }
}

public struct ArkForgeJobEvent: Sendable, Equatable {
  public let jobID: String
  public let sequence: UInt64
  public let kind: ArkForgeJobEventKind
  public let atEpochMs: UInt64
  public let journalRecordSHA256: [UInt8]
  public let jobState: String
  public let admission: ArkForgeStepAdmissionSnapshot?
  public let controlRequest: ArkForgeManagedControlRequest?
  public let receipt: ArkForgeActionReceiptSummary?
  public let facts: [ArkForgeKeyValue]

  public static func decode(_ body: Data) throws -> ArkForgeJobEvent {
    var reader = ProtobufReader(body)
    var jobID = ""
    var jobState = ""
    var sequence: UInt64 = 0
    var at: UInt64 = 0
    var kind: ArkForgeJobEventKind = .unspecified
    var journal: [UInt8] = []
    var admission: ArkForgeStepAdmissionSnapshot?
    var control: ArkForgeManagedControlRequest?
    var receipt: ArkForgeActionReceiptSummary?
    var facts: [ArkForgeKeyValue] = []
    while let field = try reader.next() {
      switch field.field {
      case 1: jobID = try field.value.asString(field: 1)
      case 2: sequence = try field.value.asUInt64()
      case 3:
        kind = try decodeEnum("JobEvent", 3, field.value, ArkForgeJobEventKind.init(rawValue:))
      case 4: at = try field.value.asUInt64()
      case 5: journal = try field.value.asBytes()
      case 6: jobState = try field.value.asString(field: 6)
      case 7:
        admission = try ArkForgeStepAdmissionSnapshot.decode(
          try field.value.asBytes(), within: reader)
      case 8:
        control = try ArkForgeManagedControlRequest.decode(
          try field.value.asBytes(), within: reader)
      case 9:
        receipt = try ArkForgeActionReceiptSummary.decode(
          try field.value.asBytes(), within: reader)
      case 10: facts.append(try ArkForgeKeyValue.decode(try field.value.asBytes(), within: reader))
      default: break
      }
    }
    return ArkForgeJobEvent(
      jobID: jobID, sequence: sequence, kind: kind, atEpochMs: at, journalRecordSHA256: journal,
      jobState: jobState, admission: admission, controlRequest: control, receipt: receipt,
      facts: facts)
  }

  public static func decodeList(_ body: Data) throws -> [ArkForgeJobEvent] {
    var reader = ProtobufReader(body)
    var events: [ArkForgeJobEvent] = []
    while let field = try reader.next() {
      guard field.field == 1 else { continue }
      events.append(try decode(Data(try field.value.asBytes())))
    }
    return events
  }
}

/// The authority's answer to one admission: a permit, or a refusal.
///
/// Never both, and never neither. A refusal is an answer the daemon acts on
/// (`CancelledSafe`); silence is a different thing entirely — the snapshot
/// expires and admission runs again.
public struct ArkForgeSubmitStepPermitRequest: Sendable {
  public let jobID: String
  public let requestID: String
  public let permitCBOR: Data
  public let integrityTag: [UInt8]
  public let pairingEpoch: UInt64
  public let refusal: String?

  /// Answers with a signed permit. The bytes are the ones that were signed,
  /// carried unchanged: a permit re-encoded by a second codec is a different
  /// permit (architecture.md 8.6).
  public init(
    jobID: String, requestID: String, permitCBOR: Data, integrityTag: [UInt8],
    pairingEpoch: UInt64
  ) {
    self.jobID = jobID
    self.requestID = requestID
    self.permitCBOR = permitCBOR
    self.integrityTag = integrityTag
    self.pairingEpoch = pairingEpoch
    self.refusal = nil
  }

  /// Declines the admission, with a reason.
  public init(jobID: String, requestID: String, refusal: String) {
    self.jobID = jobID
    self.requestID = requestID
    self.permitCBOR = Data()
    self.integrityTag = []
    self.pairingEpoch = 0
    self.refusal = refusal
  }

  public var encoded: Data {
    var writer = ProtobufWriter()
    writer.string(1, jobID)
    writer.string(2, requestID)
    writer.bytes(3, permitCBOR)
    writer.bytes(4, integrityTag)
    writer.uint64(5, pairingEpoch)
    writer.string(6, refusal ?? "")
    return writer.data
  }
}

public struct ArkForgeSubmitStepPermitResponse: Sendable, Equatable {
  public let accepted: Bool
  public let rejectionCode: String
  public let rejectionMessage: String

  public static func decode(_ body: Data) throws -> ArkForgeSubmitStepPermitResponse {
    var reader = ProtobufReader(body)
    var accepted = false
    var code = ""
    var message = ""
    while let field = try reader.next() {
      switch field.field {
      case 1: accepted = try field.value.asBool()
      case 2: code = try field.value.asString(field: 2)
      case 3: message = try field.value.asString(field: 3)
      default: break
      }
    }
    return ArkForgeSubmitStepPermitResponse(
      accepted: accepted, rejectionCode: code, rejectionMessage: message)
  }
}

/// What the authority's own control channel observed.
///
/// `accepted: false` does **not** mean nothing happened. A mode change may
/// have taken effect and gone unobserved, which the daemon records as an
/// unknown outcome. To say "it definitely did not happen", say so in
/// `failureReason` with the evidence.
public struct ArkForgeSubmitManagedControlReceiptRequest: Sendable {
  public let jobID: String
  public let requestID: String
  public let action: ArkForgeManagedControlAction
  public let accepted: Bool
  public let facts: [ArkForgeKeyValue]
  public let evidenceSHA256: [UInt8]
  public let failureReason: String

  public init(
    jobID: String, requestID: String, action: ArkForgeManagedControlAction, accepted: Bool,
    facts: [ArkForgeKeyValue], evidenceSHA256: [UInt8], failureReason: String = ""
  ) {
    self.jobID = jobID
    self.requestID = requestID
    self.action = action
    self.accepted = accepted
    self.facts = facts
    self.evidenceSHA256 = evidenceSHA256
    self.failureReason = failureReason
  }

  public var encoded: Data {
    var writer = ProtobufWriter()
    writer.string(1, jobID)
    writer.string(2, requestID)
    writer.enumeration(3, action.rawValue)
    writer.bool(4, accepted)
    for fact in facts {
      writer.message(5, fact.encoded)
    }
    writer.bytes(6, evidenceSHA256)
    writer.string(7, failureReason)
    return writer.data
  }
}

public struct ArkForgeSubmitManagedControlReceiptResponse: Sendable, Equatable {
  public let accepted: Bool
  public let rejectionCode: String
  public let rejectionMessage: String

  public static func decode(_ body: Data) throws
    -> ArkForgeSubmitManagedControlReceiptResponse
  {
    var reader = ProtobufReader(body)
    var accepted = false
    var code = ""
    var message = ""
    while let field = try reader.next() {
      switch field.field {
      case 1: accepted = try field.value.asBool()
      case 2: code = try field.value.asString(field: 2)
      case 3: message = try field.value.asString(field: 3)
      default: break
      }
    }
    return ArkForgeSubmitManagedControlReceiptResponse(
      accepted: accepted, rejectionCode: code, rejectionMessage: message)
  }
}

public struct ArkForgeCancelJobRequest: Sendable {
  public let jobID: String

  public init(jobID: String) { self.jobID = jobID }

  public var encoded: Data {
    var writer = ProtobufWriter()
    writer.string(1, jobID)
    return writer.data
  }
}

/// The cancellation state the daemon settled on.
///
/// This is where design §6.3.1 lands on the wire. `CancelledSafe` is the
/// positive answer — no tool was ever spawned, so the device is provably
/// untouched. A refusal arrives as a non-OK `Response` with code
/// `CANCEL_NOT_SAFE` instead, and that is **not** an unconfirmed cancellation:
/// it means the write is still running and will produce its own receipt.
public struct ArkForgeCancelJobResponse: Sendable, Equatable {
  public let cancellationState: String

  public static func decode(_ body: Data) throws -> ArkForgeCancelJobResponse {
    var reader = ProtobufReader(body)
    var state = ""
    while let field = try reader.next() {
      if field.field == 1 { state = try field.value.asString(field: 1) }
    }
    return ArkForgeCancelJobResponse(cancellationState: state)
  }
}

/// Durable point-in-time progress, available independently of the event
/// cursor and reconstructed from ArkForge's journal after a daemon restart.
public struct ArkForgeGetJobRequest: Sendable {
  public let jobID: String

  public init(jobID: String) {
    self.jobID = jobID
  }

  public var encoded: Data {
    var writer = ProtobufWriter()
    writer.string(1, jobID)
    return writer.data
  }
}

public struct ArkForgeJobSummary: Sendable, Equatable {
  public let jobID: String
  public let planID: String
  public let planSHA256: [UInt8]
  public let state: String
  public let terminal: Bool
  public let currentStepID: String
  public let completedSteps: UInt64
  public let totalSteps: UInt64
  public let lastSequence: UInt64
  public let stoppedReason: String

  static func decode(_ body: [UInt8], within reader: ProtobufReader) throws
    -> ArkForgeJobSummary
  {
    var nested = try reader.nested(body)
    var jobID = ""
    var planID = ""
    var state = ""
    var stepID = ""
    var reason = ""
    var planDigest: [UInt8] = []
    var terminal = false
    var completed: UInt64 = 0
    var total: UInt64 = 0
    var sequence: UInt64 = 0
    while let field = try nested.next() {
      switch field.field {
      case 1: jobID = try field.value.asString(field: 1)
      case 2: planID = try field.value.asString(field: 2)
      case 3: planDigest = try field.value.asBytes()
      case 4: state = try field.value.asString(field: 4)
      case 5: terminal = try field.value.asBool()
      case 6: stepID = try field.value.asString(field: 6)
      case 7: completed = try field.value.asUInt64()
      case 8: total = try field.value.asUInt64()
      case 9: sequence = try field.value.asUInt64()
      case 10: reason = try field.value.asString(field: 10)
      default: break
      }
    }
    return ArkForgeJobSummary(
      jobID: jobID, planID: planID, planSHA256: planDigest, state: state,
      terminal: terminal, currentStepID: stepID, completedSteps: completed,
      totalSteps: total, lastSequence: sequence, stoppedReason: reason)
  }

  public static func decodeResponse(_ body: Data) throws -> ArkForgeJobSummary {
    var reader = ProtobufReader(body)
    while let field = try reader.next() {
      guard field.field == 1 else { continue }
      return try decode(try field.value.asBytes(), within: reader)
    }
    throw ProtobufWireError.missingField(message: "GetJobResponse", field: 1)
  }

  public static func decodeList(_ body: Data) throws -> [ArkForgeJobSummary] {
    var reader = ProtobufReader(body)
    var jobs: [ArkForgeJobSummary] = []
    while let field = try reader.next() {
      guard field.field == 1 else { continue }
      jobs.append(try decode(try field.value.asBytes(), within: reader))
    }
    return jobs
  }
}
