import ArkForgeProtocol
import Foundation

/// Read-only access to ArkForge's public local endpoint.
///
/// This surface intentionally cannot start or cancel execution, submit a
/// permit, or acknowledge a managed-control action.
public final class ArkForgePublicClient: @unchecked Sendable {
  private let connection: ArkForgeLocalConnection

  public init(socketPath: String, timeoutSeconds: Int = 30) throws {
    connection = try ArkForgeLocalConnection(
      socketPath: socketPath, sessionKind: .publicSession,
      timeoutSeconds: timeoutSeconds)
  }

  public var runtimeInfo: ArkForgeHelloAck { connection.helloAck }

  public func close() {
    connection.closeSession()
  }

  public func inspectArtifact(artifactID: String, requestID: String) throws
    -> ArkForgeInspectArtifactResponse
  {
    try connection.inspectArtifact(artifactID: artifactID, requestID: requestID)
  }

  public func discoverDevices(requestID: String) throws -> [ArkForgeDeviceObservation] {
    try connection.discoverDevices(requestID: requestID)
  }

  public func materializePlan(
    _ request: ArkForgeMaterializePlanRequest, requestID: String
  ) throws -> ArkForgeMaterializePlanResponse {
    try connection.materializePlan(request, requestID: requestID)
  }

  public func getJob(jobID: String, requestID: String) throws -> ArkForgeJobSummary {
    try connection.getJob(jobID: jobID, requestID: requestID)
  }

  public func listJobs(requestID: String) throws -> [ArkForgeJobSummary] {
    try connection.listJobs(requestID: requestID)
  }
}

/// Controller access to ArkForge's local authority endpoint.
///
/// The client transports authority decisions supplied by its caller. It does
/// not create permits, choose profiles/providers, or classify recovery.
public final class ArkForgeControllerClient: @unchecked Sendable {
  private let connection: ArkForgeLocalConnection

  public static let materializationTimeoutSeconds =
    ArkForgeLocalConnection.materializationTimeoutSeconds

  public init(socketPath: String, timeoutSeconds: Int = 30) throws {
    connection = try ArkForgeLocalConnection(
      socketPath: socketPath, sessionKind: .controller,
      timeoutSeconds: timeoutSeconds)
  }

  public var runtimeInfo: ArkForgeHelloAck { connection.helloAck }
  public var helloAck: ArkForgeHelloAck { connection.helloAck }

  public func close() {
    connection.closeSession()
  }

  public func startExecution(
    _ request: ArkForgeStartExecutionRequest, requestID: String
  ) throws -> ArkForgeStartExecutionResponse {
    try connection.startExecution(request, requestID: requestID)
  }

  public func submitStepPermit(
    _ request: ArkForgeSubmitStepPermitRequest, requestID: String
  ) throws -> ArkForgeSubmitStepPermitResponse {
    try connection.submitStepPermit(request, requestID: requestID)
  }

  public func submitManagedControlReceipt(
    _ request: ArkForgeSubmitManagedControlReceiptRequest, requestID: String
  ) throws -> ArkForgeSubmitManagedControlReceiptResponse {
    try connection.submitManagedControlReceipt(request, requestID: requestID)
  }

  public func cancelJob(jobID: String, requestID: String) throws -> ArkForgeCancelJobResponse {
    try connection.cancelJob(jobID: jobID, requestID: requestID)
  }

  public func getJob(jobID: String, requestID: String) throws -> ArkForgeJobSummary {
    try connection.getJob(jobID: jobID, requestID: requestID)
  }

  public func listJobs(requestID: String) throws -> [ArkForgeJobSummary] {
    try connection.listJobs(requestID: requestID)
  }

  public func importArtifact(
    contentsOf url: URL, expectedSHA256: String, requestID: String
  ) throws -> ArkForgeImportArtifactResponse {
    try connection.importArtifact(
      contentsOf: url, expectedSHA256: expectedSHA256, requestID: requestID)
  }

  public func inspectArtifact(artifactID: String, requestID: String) throws
    -> ArkForgeInspectArtifactResponse
  {
    try connection.inspectArtifact(artifactID: artifactID, requestID: requestID)
  }

  public func discoverDevices(requestID: String) throws -> [ArkForgeDeviceObservation] {
    try connection.discoverDevices(requestID: requestID)
  }

  public func materializePlan(
    _ request: ArkForgeMaterializePlanRequest, requestID: String
  ) throws -> ArkForgeMaterializePlanResponse {
    try connection.materializePlan(request, requestID: requestID)
  }

  public func watchJob(
    _ request: ArkForgeWatchJobRequest, requestID: String,
    handle: (ArkForgeJobEvent) throws -> Bool
  ) throws {
    try connection.watchJob(request, requestID: requestID, handle: handle)
  }
}
