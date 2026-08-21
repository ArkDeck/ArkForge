import Foundation
import XCTest

@testable import ArkForgeProtocol

final class CrossLanguageGoldenTests: XCTestCase {
  private func bytes(_ hex: String) -> Data {
    var output: [UInt8] = []
    var index = hex.startIndex
    while index < hex.endIndex {
      let next = hex.index(index, offsetBy: 2)
      output.append(UInt8(hex[index..<next], radix: 16)!)
      index = next
    }
    return Data(output)
  }

  func testHandshakeAndRequestMatchRustVectors() {
    XCTAssertEqual(
      ArkForgeHello(sessionKind: .controller).encoded,
      bytes("08011802"))
    XCTAssertEqual(
      ArkForgeRequest(
        requestID: "REQ-1", api: .discoverDevices, payload: Data()
      ).encoded,
      bytes("0a055245512d311003"))
  }

  func testPlanRequestMatchesRustVector() {
    let request = ArkForgeMaterializePlanRequest(
      artifactID: "A", profileID: "P", observationID: "O",
      intent: "fullRestore", toolchainID: "T", authorityNamespace: "N",
      bindingID: "B", bindingRevision: 7, stableIdentitySHA256: [0xaa, 0xbb],
      executionPurpose: "primary")
    XCTAssertEqual(
      request.encoded,
      bytes(
        "0a01411201501a014f220b66756c6c526573746f72652a015432014e3a01424007"
          + "4a02aabb52077072696d617279"))
  }

  func testPermitAndManagedReceiptMatchRustVectors() {
    let permit = ArkForgeSubmitStepPermitRequest(
      jobID: "J", requestID: "R", permitCBOR: bytes("a1616101"),
      integrityTag: [0xab, 0xcd], pairingEpoch: 7)
    XCTAssertEqual(
      permit.encoded,
      bytes("0a014a1201521a04a16161012202abcd2807"))

    let receipt = ArkForgeSubmitManagedControlReceiptRequest(
      jobID: "J", requestID: "R", action: .readBuildFacts, accepted: true,
      facts: [ArkForgeKeyValue(key: "build", value: "1")],
      evidenceSHA256: [0x01, 0x02])
    XCTAssertEqual(
      receipt.encoded,
      bytes("0a014a120152180420012a0a0a056275696c6412013132020102"))
  }

  func testEventAndErrorDecodeRustVectors() throws {
    let event = try ArkForgeJobEvent.decode(
      bytes("0a014a1002180120032a0101320772756e6e696e67"))
    XCTAssertEqual(event.jobID, "J")
    XCTAssertEqual(event.sequence, 2)
    XCTAssertEqual(event.kind, .stateChanged)
    XCTAssertEqual(event.atEpochMs, 3)
    XCTAssertEqual(event.journalRecordSHA256, [0x01])
    XCTAssertEqual(event.jobState, "running")

    let response = try ArkForgeResponse.decode(
      bytes("0a01521006180222070a014512026e6f3001"))
    XCTAssertEqual(response.status, .refused)
    XCTAssertEqual(try response.errorBody(), ArkForgeError(code: "E", message: "no"))
  }
}
