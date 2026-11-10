import App
import AppKit
import SwiftUI
import XCTest

/// The list and the grid as AppKit draws them, driven as a user drives them:
/// with mouse and key events handed to the views, in a window off screen.
@MainActor
final class FileViewTests: XCTestCase {
    private var window: NSWindow?
    private var dir: URL!
    private var folder: URL!

    override func setUp() async throws {
        setenv("DIRFLEET_NO_DOWNLOADS", "1", 1)
        let fm = FileManager.default
        dir = fm.temporaryDirectory.appendingPathComponent("dirfleet_views_\(UUID().uuidString)")
        folder = dir.appendingPathComponent("files")
        try fm.createDirectory(at: folder, withIntermediateDirectories: true)
    }

    override func tearDown() async throws {
        window?.orderOut(nil)
        window = nil
        try? FileManager.default.removeItem(at: dir)
    }

    private func waitFor(_ what: String, _ done: () -> Bool) async throws {
        let deadline = Date().addingTimeInterval(10)
        while !done() {
            XCTAssertLessThan(Date(), deadline, "timed out waiting for \(what)")
            if Date() >= deadline { return }
            try await Task.sleep(for: .milliseconds(10))
        }
    }

    /// A core on a folder of `count` text files and one folder, shown as
    /// `display` in a window; the core and the tab's id.
    private func open(_ count: Int, display: DisplayType) async throws -> (Core, UInt32) {
        let fm = FileManager.default
        try fm.createDirectory(at: folder.appendingPathComponent("a folder"), withIntermediateDirectories: true)
        for number in 1...count {
            try Data("\(number)".utf8).write(to: folder.appendingPathComponent(String(format: "file %04d.txt", number)))
        }
        let core = Core(picker: nil, stateFile: dir.appendingPathComponent("state.json").path)
        var id: UInt32?
        core.start(open: folder.path, home: folder.path) { id = $0 }
        let tab = { core.window(id)?.active }
        try await waitFor("the listing") { tab()?.totalRows == UInt32(count + 1) }
        let number = try XCTUnwrap(tab()?.id)
        core.update(.setDisplay(tab: number, display: display))
        // Asking for the sort a tab has reverses it.
        if tab()?.view.sort != .name { core.update(.sortBy(tab: number, sort: .name)) }
        try await waitFor("the display") { tab()?.view.display == display && tab()?.view.sort == .name }
        let shown = NSWindow(contentRect: NSRect(x: 0, y: 0, width: 900, height: 700),
                             styleMask: [.titled], backing: .buffered, defer: false)
        shown.contentView = NSHostingView(rootView: FolderView(core: core, store: try XCTUnwrap(core.view.tabs[number])))
        shown.setFrameOrigin(NSPoint(x: -20000, y: -20000))
        shown.orderFrontRegardless()
        window = shown
        try await Task.sleep(for: .milliseconds(200))
        shown.displayIfNeeded()
        return (core, number)
    }

    private func grid() throws -> FileGridView {
        try XCTUnwrap(window?.contentView.flatMap { FolderViewTests.find(FileGridView.self, in: $0) })
    }

    private func table() throws -> FileTableView {
        try XCTUnwrap(window?.contentView.flatMap { FolderViewTests.find(FileTableView.self, in: $0) })
    }

    private func mouse(_ type: NSEvent.EventType, at point: NSPoint, in view: NSView, flags: NSEvent.ModifierFlags = [], clicks: Int = 1) throws -> NSEvent {
        try XCTUnwrap(NSEvent.mouseEvent(
            with: type, location: view.convert(point, to: nil), modifierFlags: flags,
            timestamp: ProcessInfo.processInfo.systemUptime, windowNumber: view.window?.windowNumber ?? 0,
            context: nil, eventNumber: 0, clickCount: clicks, pressure: 1
        ))
    }

    private func key(_ code: UInt16, _ characters: String = "", flags: NSEvent.ModifierFlags = []) throws -> NSEvent {
        try XCTUnwrap(NSEvent.keyEvent(
            with: .keyDown, location: .zero, modifierFlags: flags, timestamp: ProcessInfo.processInfo.systemUptime,
            windowNumber: window?.windowNumber ?? 0, context: nil, characters: characters,
            charactersIgnoringModifiers: characters, isARepeat: false, keyCode: code
        ))
    }

    /// Clicks tile `index` of the grid: button down, then up.
    private func click(_ index: UInt32, in grid: FileGridView, flags: NSEvent.ModifierFlags = []) throws {
        let origin = grid.placement.origin(of: index)
        let point = NSPoint(x: origin.x + 20, y: origin.y + 20)
        grid.mouseDown(with: try mouse(.leftMouseDown, at: point, in: grid, flags: flags))
        grid.mouseUp(with: try mouse(.leftMouseUp, at: point, in: grid, flags: flags))
    }

    private func selectedRows(_ core: Core, _ tab: UInt32) -> [UInt32] {
        (core.tab(tab)?.selection ?? []).flatMap { Array($0.start..<$0.end) }
    }

    private func drawnSelected(_ grid: FileGridView) -> [UInt32] {
        grid.tiles.filter { !$0.isHidden && $0.isAccessibilitySelected() }.map(\.index).sorted()
    }

    // MARK: Grid

    /// A click selects in the core and is drawn before the click returns;
    /// Shift extends, ⌘ toggles, and empty space selects nothing.
    func testClickingGridTilesSelectsThemAtOnce() async throws {
        let (core, tab) = try await open(40, display: .icons)
        let grid = try grid()
        try click(3, in: grid)
        XCTAssertEqual(selectedRows(core, tab), [3])
        XCTAssertEqual(drawnSelected(grid), [3], "drawn in the same turn")
        try click(6, in: grid, flags: .shift)
        XCTAssertEqual(selectedRows(core, tab), [3, 4, 5, 6])
        XCTAssertEqual(drawnSelected(grid), [3, 4, 5, 6])
        try click(4, in: grid, flags: .command)
        XCTAssertEqual(selectedRows(core, tab), [3, 5, 6])
        // A plain click on a selected tile selects it alone once the button is up.
        try click(5, in: grid)
        XCTAssertEqual(selectedRows(core, tab), [5])
        let below = NSPoint(x: 20, y: grid.placement.height + 40)
        grid.mouseDown(with: try mouse(.leftMouseDown, at: below, in: grid))
        XCTAssertEqual(selectedRows(core, tab), [])
        XCTAssertEqual(drawnSelected(grid), [])
    }

    /// Arrows move the selection by a tile and by a row; Shift extends.
    func testArrowKeysMoveThroughTheGrid() async throws {
        let (core, tab) = try await open(40, display: .icons)
        let grid = try grid()
        let columns = grid.placement.columns
        XCTAssertGreaterThan(columns, 2)
        try click(1, in: grid)
        grid.keyDown(with: try key(124))
        XCTAssertEqual(selectedRows(core, tab), [2])
        grid.keyDown(with: try key(125))
        XCTAssertEqual(selectedRows(core, tab), [2 + columns])
        grid.keyDown(with: try key(123, flags: .shift))
        XCTAssertEqual(selectedRows(core, tab), [1 + columns, 2 + columns])
        grid.keyDown(with: try key(115))
        XCTAssertEqual(selectedRows(core, tab), [0])
        grid.keyDown(with: try key(119))
        XCTAssertEqual(selectedRows(core, tab), [40])
        XCTAssertEqual(drawnSelected(grid), [40], "the last tile was scrolled into sight")
    }

    /// A right-click on a tile outside the selection selects it, and the
    /// menu is that row's; empty space has the folder's menu.
    func testTheGridBuildsItsMenusWhenTheyOpen() async throws {
        let (core, tab) = try await open(40, display: .icons)
        let grid = try grid()
        try click(2, in: grid)
        let origin = grid.placement.origin(of: 7)
        let onTile = try mouse(.rightMouseDown, at: NSPoint(x: origin.x + 20, y: origin.y + 20), in: grid)
        let rowMenu = try XCTUnwrap(grid.menu(for: onTile))
        XCTAssertEqual(selectedRows(core, tab), [7])
        let titles = rowMenu.items.map(\.title)
        XCTAssertEqual(Array(titles.prefix(3)), ["Open", "Open With…", "Quick Look"])
        XCTAssertTrue(titles.contains("Rename"))
        XCTAssertTrue(titles.contains("Copy To"))
        XCTAssertNotNil(rowMenu.items.first { $0.title == "Copy To" }?.submenu?.items.first { $0.title == "Choose Folder…" })

        let empty = try mouse(.rightMouseDown, at: NSPoint(x: 20, y: grid.placement.height + 40), in: grid)
        let folderMenu = try XCTUnwrap(grid.menu(for: empty))
        let folderTitles = folderMenu.items.map(\.title)
        XCTAssertEqual(Array(folderTitles.prefix(3)), ["New Folder", "New Text Document", "Paste"])
        XCTAssertTrue(folderTitles.contains("Refresh"))
        let view = try XCTUnwrap(folderMenu.items.first { $0.title == "View" }?.submenu)
        XCTAssertEqual(view.items.first { $0.title == "Name" }?.state, .on)
        // Choosing an item runs it.
        let hidden = try XCTUnwrap(view.items.first { $0.title == "Show hidden items" })
        XCTAssertEqual(hidden.state, .off)
        XCTAssertTrue(NSApp.sendAction(try XCTUnwrap(hidden.action), to: hidden.target, from: hidden))
        try await waitFor("hidden items") { core.tab(tab)?.view.showHidden == true }
    }

    /// The pool of tile views is as big as what fits and doesn't grow with
    /// the folder or with scrolling.
    func testTheGridKeepsAFixedPoolOfTiles() async throws {
        let (_, _) = try await open(400, display: .icons)
        let grid = try grid()
        let scroll = try XCTUnwrap(grid.enclosingScrollView)
        let pool = grid.tiles.count
        XCTAssertGreaterThan(pool, 0)
        XCTAssertLessThan(pool, 120)
        let views = Set(grid.tiles.map(ObjectIdentifier.init))
        for step in 1...40 {
            scroll.contentView.scroll(to: NSPoint(x: 0, y: CGFloat(step) * 97))
            scroll.reflectScrolledClipView(scroll.contentView)
        }
        XCTAssertEqual(grid.tiles.count, pool)
        XCTAssertEqual(Set(grid.tiles.map(ObjectIdentifier.init)), views, "the same views, reused")
        let drawn = grid.tiles.filter { !$0.isHidden }.map(\.index)
        XCTAssertEqual(Set(drawn).count, drawn.count, "each tile drawn once")
    }

    /// Return opens the rename field on the tile; Return in it renames.
    func testRenamingInTheGrid() async throws {
        let (core, tab) = try await open(12, display: .icons)
        let grid = try grid()
        try click(3, in: grid)
        grid.keyDown(with: try key(36, "\r"))
        try await waitFor("the rename") { core.tab(tab)?.renaming != nil }
        try await waitFor("the field") { grid.subviews.contains { ($0 as? NSTextField)?.isEditable == true } }
        let field = try XCTUnwrap(grid.subviews.compactMap { $0 as? NSTextField }.first { $0.isEditable })
        XCTAssertEqual(field.stringValue, "file 0003.txt")
        field.stringValue = "renamed.txt"
        let handled = field.delegate?.control?(field, textView: NSTextView(), doCommandBy: #selector(NSResponder.insertNewline(_:)))
        XCTAssertEqual(handled, true)
        let renamed = folder.appendingPathComponent("renamed.txt")
        try await waitFor("the file to be renamed") { FileManager.default.fileExists(atPath: renamed.path) }
        try await waitFor("the field to go") { !grid.subviews.contains { ($0 as? NSTextField)?.isEditable == true } }
    }

    // MARK: List

    /// The table has a row for every item; rows the core hasn't sent are
    /// empty until they scroll into sight.
    func testTheListDrawsRowsAsTheyCome() async throws {
        let (core, tab) = try await open(600, display: .list)
        let table = try table()
        XCTAssertEqual(table.numberOfRows, 601)
        let name = { (row: Int) -> String in
            (table.view(atColumn: 0, row: row, makeIfNecessary: false) as? NSTableCellView)?.textField?.stringValue ?? ""
        }
        XCTAssertEqual(name(0), "a folder")
        XCTAssertEqual(name(1), "file 0001.txt")
        XCTAssertNil(core.tab(tab)?.row(at: 600), "the last row isn't sent yet")
        table.scrollRowToVisible(600)
        try await waitFor("the last rows") { name(600) == "file 0600.txt" }
        XCTAssertEqual(table.numberOfRows, 601)
    }

    /// Rows selected in the table are the core's selection, and the core's
    /// selection is the table's.
    func testTheListAndTheCoreShareTheSelection() async throws {
        let (core, tab) = try await open(40, display: .list)
        let table = try table()
        table.selectRowIndexes(IndexSet([2, 4, 5]), byExtendingSelection: false)
        XCTAssertEqual(selectedRows(core, tab), [2, 4, 5])
        core.click(tab: tab, row: 7, command: false, shift: false)
        XCTAssertEqual(table.selectedRowIndexes, IndexSet(integer: 7))
        table.selectAll(nil)
        XCTAssertEqual(core.tab(tab)?.selectedCount, 41)
        XCTAssertEqual(table.selectedRowIndexes, IndexSet(integersIn: 0..<41))
    }

    /// Clicking a header sorts in the core, and the header shows the
    /// core's sort.
    func testTheListSortsFromItsHeader() async throws {
        let (core, tab) = try await open(40, display: .list)
        let table = try table()
        XCTAssertEqual(table.tableColumns.first?.identifier.rawValue, "name")
        XCTAssertEqual(table.sortDescriptors.first?.key, "name")
        XCTAssertEqual(table.sortDescriptors.first?.ascending, true)
        table.sortDescriptors = [NSSortDescriptor(key: "size", ascending: false)]
        try await waitFor("the sort") { core.tab(tab)?.view.sort == .size && core.tab(tab)?.view.invert == true }
        core.update(.sortBy(tab: tab, sort: .modified))
        core.update(.setReverse(tab: tab, reverse: false))
        try await waitFor("the header") {
            table.sortDescriptors.first?.key == "modified" && table.sortDescriptors.first?.ascending == true
        }
    }

    /// The header's menu shows and hides the optional columns.
    func testTheListShowsTheColumnsTheCoreShows() async throws {
        let (core, _) = try await open(10, display: .list)
        let table = try table()
        let menu = try XCTUnwrap(table.headerView?.menu)
        menu.delegate?.menuNeedsUpdate?(menu)
        XCTAssertEqual(menu.items.map(\.title), core.view.optionalColumns.map(\.title))
        let kind = try XCTUnwrap(menu.items.first { $0.title == "Kind" })
        let shown = kind.state == .on
        XCTAssertEqual(table.tableColumns.contains { $0.identifier.rawValue == "kind" }, shown)
        XCTAssertTrue(NSApp.sendAction(try XCTUnwrap(kind.action), to: kind.target, from: kind))
        try await waitFor("the column") { table.tableColumns.contains { $0.identifier.rawValue == "kind" } == !shown }
    }

    /// Typing jumps to a name, in the list as in the grid.
    func testTypingSelectsByName() async throws {
        let (core, tab) = try await open(40, display: .list)
        let table = try table()
        table.keyDown(with: try key(3, "f"))
        XCTAssertEqual(selectedRows(core, tab), [1], "the first name starting with f")
        XCTAssertEqual(table.selectedRowIndexes, IndexSet(integer: 1))
    }
}

/// The loader behind the rows' thumbnails.
@MainActor
final class ThumbnailLoaderTests: XCTestCase {
    private final class Counts: @unchecked Sendable {
        private let lock = NSLock()
        private var decoded: [String] = []

        func add(_ file: String) {
            lock.lock()
            decoded.append(file)
            lock.unlock()
        }

        var files: [String] {
            lock.lock()
            defer { lock.unlock() }
            return decoded
        }
    }

    private static func image() -> CGImage {
        let context = CGContext(
            data: nil, width: 4, height: 4, bitsPerComponent: 8, bytesPerRow: 16,
            space: CGColorSpaceCreateDeviceRGB(), bitmapInfo: CGImageAlphaInfo.premultipliedLast.rawValue
        )!
        return context.makeImage()!
    }

    private func waitFor(_ what: String, _ done: () -> Bool) async throws {
        let deadline = Date().addingTimeInterval(5)
        while !done() {
            XCTAssertLessThan(Date(), deadline, "timed out waiting for \(what)")
            if Date() >= deadline { return }
            try await Task.sleep(for: .milliseconds(5))
        }
    }

    /// Two cells asking for one file decode it once; a request cancelled
    /// before its turn is never decoded; the newest request goes first.
    func testEachFileIsDecodedOnceNewestFirstUnlessCancelled() async throws {
        let counts = Counts()
        let gate = DispatchSemaphore(value: 0)
        let image = Self.image()
        let loader = ThumbnailLoader(limit: 1)
        loader.decode = { file in
            if file == "first" { gate.wait() }
            counts.add(file)
            return image
        }
        var done: [String] = []
        // The first holds the only slot while the others queue behind it.
        _ = loader.load("first") { _ in done.append("first") }
        _ = loader.load("old") { _ in done.append("old") }
        _ = loader.load("old") { _ in done.append("old again") }
        let dropped = loader.load("dropped") { _ in done.append("dropped") }
        _ = loader.load("new") { _ in done.append("new") }
        loader.cancel(dropped)
        XCTAssertEqual(loader.outstanding, 3)
        gate.signal()
        try await waitFor("the decodes") { done.count == 4 }
        XCTAssertEqual(counts.files, ["first", "new", "old"])
        XCTAssertEqual(done, ["first", "new", "old", "old again"])
        XCTAssertNotNil(loader.cached("old"))
        XCTAssertNil(loader.cached("dropped"))
        XCTAssertEqual(loader.outstanding, 0)
    }
}
