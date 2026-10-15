import CoreGraphics
import XCTest
@testable import AxPlugin

/// Synthetic payloads only: no captured window content belongs here.
final class SnapshotSerialisationTests: XCTestCase {
    private func payload(frame: [Double]) -> [String: Any] {
        [
            "app": "Fixture",
            "nodes": [["d": 0, "role": "AXWindow", "frame": frame]],
            "visited": 1,
            "truncated": false,
        ]
    }

    func testKeepsAFiniteFrame() {
        let frame = finiteFrame(CGPoint(x: 10, y: 20), CGSize(width: 300, height: 400))
        XCTAssertEqual(frame, [10, 20, 300, 400])
    }

    func testKeepsAZeroSizeFrame() {
        XCTAssertEqual(finiteFrame(.zero, .zero), [0, 0, 0, 0])
    }

    func testDropsAFrameWithANonFiniteNumber() {
        let bad: [(CGPoint, CGSize)] = [
            (CGPoint(x: CGFloat.nan, y: 0), CGSize(width: 1, height: 1)),
            (CGPoint(x: 0, y: CGFloat.infinity), CGSize(width: 1, height: 1)),
            (.zero, CGSize(width: -CGFloat.infinity, height: 1)),
            (.zero, CGSize(width: 1, height: CGFloat.nan)),
        ]
        for (point, extent) in bad {
            XCTAssertNil(finiteFrame(point, extent), "\(point) \(extent)")
        }
    }

    func testSerialisesAFinitePayload() throws {
        let json = serialiseSnapshot(payload(frame: [0, 0, 1200, 800]))
        XCTAssertFalse(json.hasPrefix("ERROR:"), json)
        let decoded = try JSONSerialization.jsonObject(with: Data(json.utf8)) as? [String: Any]
        XCTAssertEqual(decoded?["app"] as? String, "Fixture")
    }

    // Before the check this raised "Invalid number value (NaN) in JSON
    // write" inside JSONSerialization, which aborted the app. XCTest catches
    // the exception and reports it as a failure instead.
    func testSkipsAPayloadHoldingNaN() {
        XCTAssertEqual(
            serialiseSnapshot(payload(frame: [0, .nan, 1200, 800])),
            "ERROR: snapshot holds a non-JSON value")
    }

    func testSkipsAPayloadHoldingInfinity() {
        XCTAssertEqual(
            serialiseSnapshot(payload(frame: [0, 0, .infinity, -.infinity])),
            "ERROR: snapshot holds a non-JSON value")
    }
}
