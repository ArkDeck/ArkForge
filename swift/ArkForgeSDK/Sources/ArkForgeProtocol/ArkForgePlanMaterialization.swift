import Foundation

/// The four calls that put a plan in the daemon's store, and the types they
/// exchange.
///
/// # Why these were missing
///
/// `chg-2026-059` design §5.2 lists materialization as step 1 of the ArkForge
/// route — "`materializePlan` (已有的只读 API) 拿到 public plan 与 plan digest"
/// — and the implementation went straight to step 2. The engine sent
/// `planID: request.operation` and `planSHA256: materializedPlanDigest`, both
/// facts about the plan **ArkDeck** materialized through its own Rockchip
/// lane, to a daemon that resolves plans from its **own** content store.
/// Measured 2026-08-17 against a live `arkforged`:
///
/// ```
/// startExecution → PLAN_NOT_STARTABLE: no stored plan flash.dayu200
/// ```
///
/// Nothing in ArkDeck had ever put a plan there, because nothing in ArkDeck
/// could: `ArkForgeApi` coded `importArtifact` and `materializePlan` and the
/// client implemented neither (AD-026).

// MARK: - importArtifact

/// What the daemon reports after taking the bytes.
///
/// `artifactID` is the content digest: the store is content-addressed, so an
/// archive imported twice is one artifact and `deduplicated` says so rather
/// than the second import failing.
public struct ArkForgeImportArtifactResponse: Sendable, Equatable {
  public let artifactID: String
  public let contentSHA256: String
  public let sizeBytes: UInt64
  public let deduplicated: Bool

  public static func decode(_ body: Data) throws -> ArkForgeImportArtifactResponse {
    var reader = ProtobufReader(body)
    var artifactID = ""
    var contentSHA256 = ""
    var sizeBytes: UInt64 = 0
    var deduplicated = false
    while let field = try reader.next() {
      switch field.field {
      case 1: artifactID = try field.value.asString(field: 1)
      case 2: contentSHA256 = try field.value.asString(field: 2)
      case 3: sizeBytes = try field.value.asUInt64()
      case 4: deduplicated = try field.value.asBool()
      default: break
      }
    }
    return ArkForgeImportArtifactResponse(
      artifactID: artifactID, contentSHA256: contentSHA256, sizeBytes: sizeBytes,
      deduplicated: deduplicated)
  }
}

/// The header that precedes the content frames.
///
/// Both fields are stated up front so the daemon can refuse an oversized or
/// wrong-digest import before it has written anything, rather than after
/// spending the disk.
public struct ArkForgeImportArtifactRequest: Sendable {
  public let expectedSizeBytes: UInt64
  public let expectedSHA256: String

  public init(expectedSizeBytes: UInt64, expectedSHA256: String) {
    self.expectedSizeBytes = expectedSizeBytes
    self.expectedSHA256 = expectedSHA256
  }

  public var encoded: Data {
    var writer = ProtobufWriter()
    writer.uint64(1, expectedSizeBytes)
    writer.string(2, expectedSHA256)
    return writer.data
  }
}

// MARK: - inspectArtifact

public struct ArkForgeInspectArtifactRequest: Sendable {
  public let artifactID: String

  public init(artifactID: String) {
    self.artifactID = artifactID
  }

  public var encoded: Data {
    var writer = ProtobufWriter()
    writer.string(1, artifactID)
    return writer.data
  }
}

/// The subset of the manifest this lane reads.
///
/// Inspection is not optional even when nothing here is used: `materializePlan`
/// refuses with `ARTIFACT_NOT_INSPECTED` until the daemon has built the
/// artifact's manifest, and building it is what `inspectArtifact` does.
public struct ArkForgeInspectArtifactResponse: Sendable, Equatable {
  public let formatID: String
  public let contentSHA256: String
  public let sizeBytes: UInt64
  public let manifestSHA256: String
  /// Facts read out of the images themselves. The postflight expectation must
  /// come from here rather than from an archive filename — AF-011, and the
  /// 2026-07-28 daily whose name and build log both said 7.0.0.35.
  public let buildFacts: [String: String]

  public static func decode(_ body: Data) throws -> ArkForgeInspectArtifactResponse {
    var reader = ProtobufReader(body)
    var formatID = ""
    var contentSHA256 = ""
    var sizeBytes: UInt64 = 0
    var manifestSHA256 = ""
    var buildFacts: [String: String] = [:]
    while let field = try reader.next() {
      switch field.field {
      case 1: formatID = try field.value.asString(field: 1)
      case 2: contentSHA256 = try field.value.asString(field: 2)
      case 3: sizeBytes = try field.value.asUInt64()
      case 6:
        let pair = try ArkForgeKeyValue.decode(try field.value.asBytes(), within: reader)
        buildFacts[pair.key] = pair.value
      case 10: manifestSHA256 = try field.value.asString(field: 10)
      default: break
      }
    }
    return ArkForgeInspectArtifactResponse(
      formatID: formatID, contentSHA256: contentSHA256, sizeBytes: sizeBytes,
      manifestSHA256: manifestSHA256, buildFacts: buildFacts)
  }
}

// MARK: - discoverDevices

/// One device the daemon observed, in the daemon's own vocabulary.
///
/// `observationID` is opaque here on purpose — it is ArkForge's identifier for
/// a sighting and this side must not parse it. The field that carries meaning
/// across the boundary is `topologyDigest`; see
/// `ArkForgeDeviceObservation.matching`.
public struct ArkForgeDeviceObservation: Sendable, Equatable {
  public let observationID: String
  public let observedAtEpochMS: UInt64
  public let mode: String
  public let topologyDigest: String
  public let descriptorDigest: String
  public let identityStrength: String
  public let malformedDescriptor: Bool
  public let protocolIdentity: [String: String]

  static func decode(_ body: [UInt8], within reader: ProtobufReader) throws
    -> ArkForgeDeviceObservation
  {
    var nested = try reader.nested(body)
    var observationID = ""
    var observedAt: UInt64 = 0
    var mode = ""
    var topology = ""
    var descriptor = ""
    var strength = ""
    var malformed = false
    var identity: [String: String] = [:]
    while let field = try nested.next() {
      switch field.field {
      case 1: observationID = try field.value.asString(field: 1)
      case 2: observedAt = try field.value.asUInt64()
      case 3: mode = try field.value.asString(field: 3)
      case 4: topology = try field.value.asString(field: 4)
      case 5: descriptor = try field.value.asString(field: 5)
      case 6: strength = try field.value.asString(field: 6)
      case 7: malformed = try field.value.asBool()
      case 8:
        let pair = try ArkForgeKeyValue.decode(try field.value.asBytes(), within: nested)
        identity[pair.key] = pair.value
      default: break
      }
    }
    return ArkForgeDeviceObservation(
      observationID: observationID, observedAtEpochMS: observedAt, mode: mode,
      topologyDigest: topology, descriptorDigest: descriptor, identityStrength: strength,
      malformedDescriptor: malformed, protocolIdentity: identity)
  }

  public static func decodeList(_ body: Data) throws -> [ArkForgeDeviceObservation] {
    var reader = ProtobufReader(body)
    var out: [ArkForgeDeviceObservation] = []
    while let field = try reader.next() {
      if field.field == 1 {
        out.append(
          try ArkForgeDeviceObservation.decode(try field.value.asBytes(), within: reader))
      }
    }
    return out
  }
}

// MARK: - materializePlan

/// A plan the daemon stored, addressed the way the daemon addresses it.
///
/// These two strings are the whole point of the call. `startExecution` takes
/// `planID` and `planSHA256`, and until they come from here they name
/// something the daemon's store has never heard of.
public struct ArkForgeExecutablePlan: Sendable, Equatable {
  public let planID: String
  public let planSHA256: String
  public let providerExecutionPlanSHA256: String
  public let publicProjectionSHA256: String
  public let expiresAtEpochMS: UInt64
  public let executionPurpose: String

  static func decode(_ body: [UInt8], within reader: ProtobufReader) throws
    -> ArkForgeExecutablePlan
  {
    var nested = try reader.nested(body)
    var planID = ""
    var planSHA256 = ""
    var providerDigest = ""
    var projectionDigest = ""
    var expiresAt: UInt64 = 0
    var executionPurpose = ""
    while let field = try nested.next() {
      switch field.field {
      case 1: planID = try field.value.asString(field: 1)
      case 2: planSHA256 = try field.value.asString(field: 2)
      case 3: providerDigest = try field.value.asString(field: 3)
      case 4: projectionDigest = try field.value.asString(field: 4)
      case 9: expiresAt = try field.value.asUInt64()
      case 10: executionPurpose = try field.value.asString(field: 10)
      default: break
      }
    }
    return ArkForgeExecutablePlan(
      planID: planID, planSHA256: planSHA256,
      providerExecutionPlanSHA256: providerDigest,
      publicProjectionSHA256: projectionDigest, expiresAtEpochMS: expiresAt,
      executionPurpose: executionPurpose)
  }
}

/// Why the daemon materialized an assessment instead of a plan.
///
/// Not an error. An assessment is the daemon saying it built the whole plan and
/// then declined to make it executable, and the reasons are the useful part:
/// `availability` names the gate and `unknowns` names each blocker. A maturity
/// that is not `productionVerified` or `hardwareCampaign` shows up here.
public struct ArkForgePlanAssessment: Sendable, Equatable {
  public let availability: String
  public let unavailableReason: String
  public let unknowns: [String: String]

  static func decode(_ body: [UInt8], within reader: ProtobufReader) throws
    -> ArkForgePlanAssessment
  {
    var nested = try reader.nested(body)
    var availability = ""
    var reason = ""
    var unknowns: [String: String] = [:]
    while let field = try nested.next() {
      switch field.field {
      case 3:
        let pair = try ArkForgeKeyValue.decode(try field.value.asBytes(), within: nested)
        unknowns[pair.key] = pair.value
      case 5: availability = try field.value.asString(field: 5)
      case 6: reason = try field.value.asString(field: 6)
      default: break
      }
    }
    return ArkForgePlanAssessment(
      availability: availability, unavailableReason: reason, unknowns: unknowns)
  }
}

/// Exactly one side is populated.
public enum ArkForgeMaterializePlanResponse: Sendable, Equatable {
  case plan(ArkForgeExecutablePlan)
  case assessment(ArkForgePlanAssessment)

  public static func decode(_ body: Data) throws -> ArkForgeMaterializePlanResponse {
    var reader = ProtobufReader(body)
    var plan: ArkForgeExecutablePlan?
    var assessment: ArkForgePlanAssessment?
    while let field = try reader.next() {
      switch field.field {
      case 1:
        plan = try ArkForgeExecutablePlan.decode(try field.value.asBytes(), within: reader)
      case 2:
        assessment = try ArkForgePlanAssessment.decode(
          try field.value.asBytes(), within: reader)
      default: break
      }
    }
    switch (plan, assessment) {
    case (let plan?, nil): return .plan(plan)
    case (nil, let assessment?): return .assessment(assessment)
    default:
      // Both or neither is a message the schema cannot mean, and guessing
      // which was intended would be guessing whether a device may be written.
      throw ProtobufWireError.missingField(message: "MaterializePlanResponse", field: 1)
    }
  }
}

public struct ArkForgeMaterializePlanRequest: Sendable {
  public let artifactID: String
  public let profileID: String
  public let observationID: String
  public let intent: String
  public let toolchainID: String
  public let authorityNamespace: String
  public let bindingID: String
  public let bindingRevision: UInt64
  public let stableIdentitySHA256: [UInt8]
  public let executionPurpose: String

  public init(
    artifactID: String, profileID: String, observationID: String,
    intent: String, toolchainID: String, authorityNamespace: String,
    bindingID: String, bindingRevision: UInt64, stableIdentitySHA256: [UInt8],
    executionPurpose: String
  ) {
    self.artifactID = artifactID
    self.profileID = profileID
    self.observationID = observationID
    self.intent = intent
    self.toolchainID = toolchainID
    self.authorityNamespace = authorityNamespace
    self.bindingID = bindingID
    self.bindingRevision = bindingRevision
    self.stableIdentitySHA256 = stableIdentitySHA256
    self.executionPurpose = executionPurpose
  }

  public var encoded: Data {
    var writer = ProtobufWriter()
    writer.string(1, artifactID)
    writer.string(2, profileID)
    writer.string(3, observationID)
    writer.string(4, intent)
    writer.string(5, toolchainID)
    writer.string(6, authorityNamespace)
    writer.string(7, bindingID)
    writer.uint64(8, bindingRevision)
    writer.bytes(9, stableIdentitySHA256)
    writer.string(10, executionPurpose)
    return writer.data
  }
}
