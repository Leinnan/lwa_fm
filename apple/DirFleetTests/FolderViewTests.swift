import App
import AppKit
import SwiftUI
import XCTest

/// Draws the folder views themselves, for what only drawing shows.
@MainActor
final class FolderViewTests: XCTestCase {
    override func setUp() {
        super.setUp()
        setenv("DIRFLEET_NO_DOWNLOADS", "1", 1)
    }

    private func waitFor(_ what: String, _ done: () -> Bool) async throws {
        let deadline = Date().addingTimeInterval(10)
        while !done() {
            XCTAssertLessThan(Date(), deadline, "timed out waiting for \(what)")
            if Date() >= deadline { return }
            try await Task.sleep(for: .milliseconds(10))
        }
    }

    /// Drawing the grid changes nothing in the core: the clicked tile stays
    /// the selected one (building each tile's menu used to select every
    /// drawn tile in turn), and its tile is the one drawn selected.
    func testDrawingTheIconGridKeepsTheClickedTileSelected() async throws {
        let fm = FileManager.default
        let dir = fm.temporaryDirectory.appendingPathComponent("dirfleet_grid_\(UUID().uuidString)")
        let folder = dir.appendingPathComponent("files")
        try fm.createDirectory(at: folder, withIntermediateDirectories: true)
        defer { try? fm.removeItem(at: dir) }
        for number in 1...30 {
            try Data("\(number)".utf8).write(to: folder.appendingPathComponent(String(format: "file %02d.txt", number)))
        }

        let core = Core(picker: nil, stateFile: dir.appendingPathComponent("state.json").path)
        var window: UInt32?
        core.start(open: folder.path, home: folder.path) { window = $0 }
        let tab = { core.window(window)?.active }
        try await waitFor("the listing") { tab()?.totalRows == 30 }
        let id = try XCTUnwrap(tab()?.id)
        core.update(.setDisplay(tab: id, display: .icons))
        core.update(.viewportChanged(tab: id, firstRow: 0, rows: 30))
        try await waitFor("the grid") { tab()?.view.display == .icons && tab()?.rows.count == 30 }
        core.update(.click(tab: id, row: 3, command: false, shift: false))
        let selected = { tab()?.rows.filter(\.selected).map(\.name) ?? [] }
        try await waitFor("the click") { selected() == ["file 04.txt"] }

        let shown = NSWindow(contentRect: NSRect(x: 0, y: 0, width: 900, height: 700),
                             styleMask: [.titled], backing: .buffered, defer: false)
        shown.contentView = NSHostingView(rootView: FolderView(core: core, store: try XCTUnwrap(core.view.tabs[id])))
        shown.setFrameOrigin(NSPoint(x: -20000, y: -20000))
        shown.orderFrontRegardless()
        defer { shown.orderOut(nil) }
        for _ in 0..<3 {
            shown.displayIfNeeded()
            try await Task.sleep(for: .milliseconds(100))
        }
        XCTAssertEqual(selected(), ["file 04.txt"], "drawing changed the selection")
        XCTAssertEqual(tab()?.selectedCount, 1)
        let grid = try XCTUnwrap(shown.contentView.flatMap { Self.find(FileGridView.self, in: $0) })
        let drawn = grid.tiles.filter { !$0.isHidden && $0.isAccessibilitySelected() }.map(\.index)
        XCTAssertEqual(drawn, [3], "the clicked tile is the one drawn selected")
    }

    /// The first view of a type under `view`.
    static func find<Found: NSView>(_ type: Found.Type, in view: NSView) -> Found? {
        if let found = view as? Found { return found }
        return view.subviews.lazy.compactMap { find(type, in: $0) }.first
    }
}

extension FolderViewTests {
    /// The pointer moving over the grid's tiles tells the core which tile
    /// it is on (for playing previews), from the grid's one tracking area.
    func testMovingThePointerOverTheGridHoversTiles() async throws {
        let fm = FileManager.default
        let dir = fm.temporaryDirectory.appendingPathComponent("dirfleet_hover_\(UUID().uuidString)")
        let folder = dir.appendingPathComponent("files")
        try fm.createDirectory(at: folder, withIntermediateDirectories: true)
        defer { try? fm.removeItem(at: dir) }
        for number in 1...30 {
            try Data("\(number)".utf8).write(to: folder.appendingPathComponent(String(format: "file %02d.txt", number)))
        }
        let core = Core(picker: nil, stateFile: dir.appendingPathComponent("state.json").path)
        var window: UInt32?
        core.start(open: folder.path, home: folder.path) { window = $0 }
        let tab = { core.window(window)?.active }
        try await waitFor("the listing") { tab()?.totalRows == 30 }
        let id = try XCTUnwrap(tab()?.id)
        core.update(.setDisplay(tab: id, display: .icons))
        core.update(.viewportChanged(tab: id, firstRow: 0, rows: 30))
        try await waitFor("the grid") { tab()?.view.display == .icons && tab()?.rows.count == 30 }

        let shown = NSWindow(contentRect: NSRect(x: 0, y: 0, width: 900, height: 700),
                             styleMask: [.titled], backing: .buffered, defer: false)
        shown.contentView = NSHostingView(rootView: FolderView(core: core, store: try XCTUnwrap(core.view.tabs[id])))
        shown.setFrameOrigin(NSPoint(x: -20000, y: -20000))
        shown.orderFrontRegardless()
        defer { shown.orderOut(nil) }
        try await Task.sleep(for: .milliseconds(300))
        let before = core.stats.updates
        try await Task.sleep(for: .milliseconds(300))
        XCTAssertEqual(core.stats.updates, before, "a hover with no pointer")

        // The grid's tracking area takes the pointer's moves; AppKit sends it
        // those from the real pointer only, so they are handed to it here.
        let tracker = try XCTUnwrap(shown.contentView.flatMap { Self.find(FileGridView.self, in: $0) })
        // Across the first row of tiles (the grid starts 41 points down), in
        // window coordinates (from the bottom).
        for x in stride(from: 20.0, through: 880.0, by: 20.0) {
            let event = try XCTUnwrap(NSEvent.mouseEvent(
                with: .mouseMoved, location: NSPoint(x: x, y: 700 - 100), modifierFlags: [],
                timestamp: ProcessInfo.processInfo.systemUptime, windowNumber: shown.windowNumber,
                context: nil, eventNumber: 0, clickCount: 0, pressure: 0
            ))
            tracker.mouseMoved(with: event)
            try await Task.sleep(for: .milliseconds(10))
        }
        try await waitFor("hovers") { core.stats.updates - before >= 4 }
    }

    /// Drawing the list sends the core nothing about its columns: the table
    /// writing its customization back while it sets up used to toggle the
    /// optional columns on every write.
    func testDrawingTheListLeavesItsColumnsAlone() async throws {
        let fm = FileManager.default
        let dir = fm.temporaryDirectory.appendingPathComponent("dirfleet_columns_\(UUID().uuidString)")
        let folder = dir.appendingPathComponent("files")
        try fm.createDirectory(at: folder, withIntermediateDirectories: true)
        defer { try? fm.removeItem(at: dir) }
        for number in 1...30 {
            try Data("\(number)".utf8).write(to: folder.appendingPathComponent(String(format: "file %02d.txt", number)))
        }
        let core = Core(picker: nil, stateFile: dir.appendingPathComponent("state.json").path)
        var window: UInt32?
        core.start(open: folder.path, home: folder.path) { window = $0 }
        let tab = { core.window(window)?.active }
        try await waitFor("the listing") { tab()?.totalRows == 30 }
        let id = try XCTUnwrap(tab()?.id)
        core.update(.setDisplay(tab: id, display: .list))
        try await waitFor("the list") { tab()?.view.display == .list }
        let columns = core.view.optionalColumns
        let before = core.stats.updates

        let shown = NSWindow(contentRect: NSRect(x: 0, y: 0, width: 900, height: 700),
                             styleMask: [.titled], backing: .buffered, defer: false)
        shown.contentView = NSHostingView(rootView: WindowContent(core: core, window: .constant(window)))
        shown.setFrameOrigin(NSPoint(x: -20000, y: -20000))
        shown.orderFrontRegardless()
        defer { shown.orderOut(nil) }
        try await Task.sleep(for: .milliseconds(500))
        // Only viewport reports, a few at most.
        XCTAssertLessThan(core.stats.updates - before, 6)
        XCTAssertEqual(core.view.optionalColumns, columns)
    }

    /// The window's size limits are set by hand, not measured from its
    /// content after every change.
    func testTheWindowIsSizedByHand() async throws {
        let fm = FileManager.default
        let dir = fm.temporaryDirectory.appendingPathComponent("dirfleet_window_\(UUID().uuidString)")
        try fm.createDirectory(at: dir, withIntermediateDirectories: true)
        defer { try? fm.removeItem(at: dir) }
        let core = Core(picker: nil, stateFile: dir.appendingPathComponent("state.json").path)
        var id: UInt32?
        core.start(open: dir.path, home: dir.path) { id = $0 }
        try await waitFor("a window") { core.window(id) != nil }
        let window = NSWindow(contentRect: NSRect(x: 0, y: 0, width: 900, height: 700),
                              styleMask: [.titled, .resizable], backing: .buffered, defer: false)
        let hosting = NSHostingView(rootView: WindowContent(core: core, window: .constant(id)))
        window.contentView = hosting
        window.setFrameOrigin(NSPoint(x: -20000, y: -20000))
        window.orderFrontRegardless()
        defer { window.orderOut(nil) }
        try await waitFor("the window's limits") { window.contentMinSize == WindowContent.minSize }
        XCTAssertEqual(hosting.sizingOptions, [], "the hosting view still measures the window")
    }

    /// Draws the path bar of a deep folder at a few widths, to
    /// `DIRFLEET_SNAPSHOT_DIR`, for looking at how crumbs fold.
    func testPathBarSnapshots() async throws {
        guard let out = ProcessInfo.processInfo.environment["DIRFLEET_SNAPSHOT_DIR"] else {
            throw XCTSkip("only with DIRFLEET_SNAPSHOT_DIR")
        }
        let fm = FileManager.default
        let dir = fm.temporaryDirectory.appendingPathComponent("dirfleet_crumbs_\(UUID().uuidString)")
        let deep = dir.appendingPathComponent("Projects/Clients/Acme Corporation/Design/Final Assets/2026")
        try fm.createDirectory(at: deep, withIntermediateDirectories: true)
        defer { try? fm.removeItem(at: dir) }
        let core = Core(picker: nil, stateFile: dir.appendingPathComponent("state.json").path)
        var window: UInt32?
        core.start(open: deep.path, home: dir.path) { window = $0 }
        try await waitFor("the folder") { core.window(window)?.active.crumbs.count ?? 0 > 3 }
        let tab = try XCTUnwrap(core.window(window)?.active)
        for width in [250.0, 450.0, 900.0] {
            let renderer = ImageRenderer(content: PathBar(core: core, tab: tab).frame(width: width).padding(4))
            renderer.scale = 2
            let image = try XCTUnwrap(renderer.nsImage)
            let bitmap = try XCTUnwrap(NSBitmapImageRep(data: try XCTUnwrap(image.tiffRepresentation)))
            try bitmap.representation(using: .png, properties: [:])?.write(to: URL(fileURLWithPath: "\(out)/crumbs-\(Int(width)).png"))
        }
    }

    /// The grid draws a fixed pool of tiles: scrolling a row frees exactly
    /// the slots the next row takes, so those tile views are reused.
    func testTheGridReusesTheSlotsOfARowScrolledAway() {
        let layout = TilePlacement(width: 1000, tile: CGSize(width: 104, height: 104), spacing: 12, padding: 12, count: 847)
        let height: CGFloat = 700
        let pool = layout.pool(height: height)
        XCTAssertEqual(pool % layout.columns, 0, "whole rows")
        var y: CGFloat = 0
        var last = layout.items(from: 0, height: height)
        while y < layout.height - height {
            y += 29
            let items = layout.items(from: y, height: height)
            // All of it, short only of the last row's missing tiles.
            XCTAssertTrue(UInt32(items.count) == pool || items.upperBound == 847, "the whole pool at \(y)")
            XCTAssertTrue(items.contains(layout.inSight(from: y, height: height).lowerBound), "what is in sight at \(y)")
            XCTAssertTrue(items.contains(layout.inSight(from: y, height: height).upperBound - 1), "what is in sight at \(y)")
            // The new tiles take the slots the old ones left.
            let slots = Set(items.map { $0 % pool })
            XCTAssertEqual(slots.count, items.count, "slots are unique at \(y)")
            let before = Set(last.map { $0 % pool })
            XCTAssertTrue(slots.isSubset(of: before) || before.isSubset(of: slots), "the same slots at \(y)")
            last = items
        }
        XCTAssertEqual(last.upperBound, 847, "down to the last tile")
    }

    /// Hovering finds the tile under the pointer from the layout alone.
    func testTheGridFindsTheTileUnderThePointer() {
        let layout = TilePlacement(width: 1000, tile: CGSize(width: 104, height: 104), spacing: 12, padding: 12, count: 20)
        let columns = layout.columns
        for index: UInt32 in [0, 1, columns - 1, columns, 19] {
            let origin = layout.origin(of: index)
            XCTAssertEqual(layout.index(at: CGPoint(x: origin.x + 1, y: origin.y + 1)), index)
            XCTAssertEqual(layout.index(at: CGPoint(x: origin.x + 103, y: origin.y + 103)), index)
        }
        let first = layout.origin(of: 0)
        XCTAssertNil(layout.index(at: CGPoint(x: first.x - 1, y: first.y + 50)), "left of the first tile")
        XCTAssertNil(layout.index(at: CGPoint(x: first.x + 50, y: first.y + 110)), "in the gap below it")
        XCTAssertNil(layout.index(at: CGPoint(x: 995, y: first.y + 50)), "past the last column")
        let below = layout.origin(of: 19)
        XCTAssertNil(layout.index(at: CGPoint(x: first.x + 1, y: below.y + 116 + 1)), "below the last row")
        XCTAssertNil(layout.index(at: CGPoint(x: layout.origin(of: 19).x + 116 + 1, y: below.y + 1)), "after the last tile")
    }
}
