import App
import Foundation
import Shared
import XCTest

private final class RenderCounter: CruxShell, @unchecked Sendable {
    private let lock = NSLock()
    private var count = 0

    func processEffects(bytes _: Data) {
        lock.lock()
        count += 1
        lock.unlock()
    }
}

/// The spike gate, measured from Swift with the 100k-file folder open in 10
/// windows: a viewport change through the bridge including encoding the
/// event and decoding the view model.
final class BridgeLatencyTests: XCTestCase {
    /// Tests never download FFmpeg.
    override class func setUp() {
        super.setUp()
        setenv("DIRFLEET_NO_DOWNLOADS", "1", 1)
    }

    private func fixture() throws -> String {
        let dir = URL(fileURLWithPath: NSTemporaryDirectory()).appendingPathComponent("lwa_fm_perf_flat_100000")
        guard FileManager.default.fileExists(atPath: dir.appendingPathComponent(".complete").path) else {
            throw XCTSkip("run `cargo test --release -p dirfleet_ffi --test latency -- --ignored` first to create \(dir.path)")
        }
        return dir.path
    }

    private func view(_ core: CoreFfi) throws -> ViewModel {
        try ViewModel.bincodeDeserialize(input: [UInt8](core.view()))
    }

    private func allListed(_ view: ViewModel, windows: UInt32) -> Bool {
        view.windows.count == Int(windows) && view.windows.allSatisfy { $0.active.totalRows >= 50000 }
    }

    func testViewportRoundTripOnA100kFolder() throws {
        let folder = try fixture()
        let core = CoreFfi(shell: RenderCounter())
        let windows: UInt32 = 10
        for _ in 0..<windows {
            _ = core.update(data: Data(try Event.newWindow(path: folder).bincodeSerialize()))
        }
        let deadline = Date().addingTimeInterval(60)
        while try !allListed(view(core), windows: windows) {
            XCTAssertLessThan(Date(), deadline, "listing timed out")
            Thread.sleep(forTimeInterval: 0.01)
        }

        var times: [Double] = []
        for run in 0..<1000 {
            let first = UInt32(run * 97 % 100_000)
            // Tab ids start at 1, one tab per window.
            let tab = UInt32(run) % windows + 1
            let start = DispatchTime.now().uptimeNanoseconds
            _ = core.update(data: Data(
                try Event.viewportChanged(tab: tab, firstRow: first, rows: 40).bincodeSerialize()
            ))
            let model = try view(core)
            times.append(Double(DispatchTime.now().uptimeNanoseconds - start) / 1_000_000)
            let shown = model.windows.first { $0.active.id == tab }?.active
            // 40 visible: sent in steps of 20 rows, from a screen before.
            let step = first / 20 * 20
            XCTAssertEqual(shown?.firstRow, step >= 40 ? step - 40 : 0)
        }
        times.sort()
        let p50 = times[times.count / 2]
        let p95 = times[Int(Double(times.count - 1) * 0.95)]
        print(String(format: "spike swift update+view+decode p50 %.3f ms p95 %.3f ms", p50, p95))
        XCTAssertLessThan(p95, 8, "gate: bridge round trip p95 under 8 ms")
    }
}
