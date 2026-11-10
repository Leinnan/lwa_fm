import App
import Foundation
import Shared
import XCTest

private final class NoShell: CruxShell, @unchecked Sendable {
    func processEffects(bytes _: Data) {}
}

private struct Script: Decodable {
    struct Fixture: Decodable {
        struct Entry: Decodable {
            let path: String
            let dir: Bool?
            let size: Int?
        }

        let root: String
        let modified: Double
        let entries: [Entry]
    }

    /// Only the fixture is read here: the steps come from `<script>.events.bin`.
    let fixture: Fixture
}

/// A step of `<script>.events.bin` (format in `crates/dirfleet_ffi/tests/scripts.rs`).
private enum Replay {
    case event(Data)
    case snapshot
}

private func replays(_ data: Data) throws -> [Replay] {
    var steps: [Replay] = []
    var at = data.startIndex
    while at < data.endIndex {
        let tag = data[at]
        at += 1
        switch tag {
        case UInt8(ascii: "S"):
            steps.append(.snapshot)
        case UInt8(ascii: "E"):
            guard data.endIndex - at >= 4 else { throw CocoaError(.fileReadCorruptFile) }
            let length = data[at..<at + 4].enumerated()
                .reduce(0) { $0 | Int($1.element) << (8 * $1.offset) }
            at += 4
            guard data.endIndex - at >= length else { throw CocoaError(.fileReadCorruptFile) }
            steps.append(.event(Data(data[at..<at + length])))
            at += length
        default:
            throw CocoaError(.fileReadCorruptFile)
        }
    }
    return steps
}

/// The same scripts and golden files as `crates/dirfleet_ffi/tests/scripts.rs`:
/// the Swift shell must see exactly the view models the Rust test recorded.
/// It replays the events as bytes, so no script needs Swift code.
final class ScriptTests: XCTestCase {
    /// Tests never download FFmpeg.
    override class func setUp() {
        super.setUp()
        setenv("DIRFLEET_NO_DOWNLOADS", "1", 1)
    }

    private var scriptsDir: URL {
        URL(fileURLWithPath: #filePath)
            .deletingLastPathComponent()
            .appendingPathComponent("../../tests/scripts")
            .standardizedFileURL
    }

    private func build(_ fixture: Script.Fixture) throws {
        let files = FileManager.default
        let root = URL(fileURLWithPath: fixture.root)
        try? files.removeItem(at: root)
        try files.createDirectory(at: root, withIntermediateDirectories: true)
        for entry in fixture.entries {
            let url = root.appendingPathComponent(entry.path)
            if entry.dir == true {
                try files.createDirectory(at: url, withIntermediateDirectories: true)
            } else {
                try Data(repeating: UInt8(ascii: "x"), count: entry.size ?? 0).write(to: url)
            }
        }
        let date = Date(timeIntervalSince1970: fixture.modified)
        let paths = fixture.entries
            .map { root.appendingPathComponent($0.path) }
            .sorted { $0.pathComponents.count > $1.pathComponents.count } + [root]
        for url in paths {
            try files.setAttributes([.modificationDate: date], ofItemAtPath: url.path)
        }
    }

    private func run(_ steps: [Replay]) throws -> Data {
        let core = CoreFfi(shell: NoShell())
        var packed = Data()
        for step in steps {
            switch step {
            case let .event(bytes):
                _ = core.update(data: bytes)
            case .snapshot:
                let deadline = Date().addingTimeInterval(10)
                while true {
                    let bytes = core.view()
                    let view = try ViewModel.bincodeDeserialize(input: [UInt8](bytes))
                    if view.windows.allSatisfy({ $0.panes.allSatisfy { !$0.shown.loading } }) {
                        var length = UInt32(bytes.count).littleEndian
                        packed.append(Data(bytes: &length, count: 4))
                        packed.append(bytes)
                        break
                    }
                    XCTAssertLessThan(Date(), deadline, "timed out loading")
                    Thread.sleep(forTimeInterval: 0.01)
                }
            }
        }
        return packed
    }

    func testScriptsMatchTheirGoldenViewModels() throws {
        let scripts = try FileManager.default
            .contentsOfDirectory(at: scriptsDir, includingPropertiesForKeys: nil)
            .filter { $0.pathExtension == "json" }
        XCTAssertFalse(scripts.isEmpty, "no scripts in \(scriptsDir.path)")
        for url in scripts {
            let script = try JSONDecoder().decode(Script.self, from: Data(contentsOf: url))
            let base = url.deletingPathExtension()
            let steps = try replays(Data(contentsOf: base.appendingPathExtension("events.bin")))
            let golden = try Data(contentsOf: base.appendingPathExtension("golden"))
            try build(script.fixture)
            XCTAssertEqual(try run(steps), golden, "\(url.lastPathComponent) differs from its golden file")
        }
    }
}
