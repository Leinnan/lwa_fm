import App
import AppKit
import Foundation
import ImageIO
import XCTest

/// Answers every pick with a fixed folder, or cancels when there is none.
private struct FakePicker: FolderPicker {
    let path: String?
    func pick(title _: String) async -> String? { path }
}

/// Records what it was asked for and cancels.
private final class AppPickerSpy: FolderPicker {
    var asked: [String] = []
    func pick(title _: String) async -> String? { nil }
    func pickApp(title: String) async -> String? {
        asked.append(title)
        return nil
    }
}

/// Effects the Swift shell handles itself, answered through `CoreFfi.resolve`.
@MainActor
final class ShellEffectTests: XCTestCase {
    /// Tests never download FFmpeg.
    override class func setUp() {
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

    func testOpenFolderShowsThePickedFolder() async throws {
        let folder = FileManager.default.temporaryDirectory
            .appendingPathComponent("dirfleet_pick_\(UUID().uuidString)")
        try FileManager.default.createDirectory(
            at: folder.appendingPathComponent("inside"), withIntermediateDirectories: true
        )
        defer { try? FileManager.default.removeItem(at: folder) }

        let core = Core(picker: FakePicker(path: folder.path))
        core.update(.newWindow(path: nil))
        core.update(.openFolder(tab: 1))
        try await waitFor("the picked folder") {
            core.window(1)?.active.path == folder.path && core.window(1)?.active.loading == false
        }
        XCTAssertEqual(core.window(1)?.active.rows.map(\.name), ["inside"])
    }

    /// The Trash is read for real: with Full Disk Access it lists, without
    /// it the tab says why. Either way the tab shows the Trash.
    func testTheTrashOpensInATabAndBackLeavesIt() async throws {
        let folder = FileManager.default.temporaryDirectory
            .appendingPathComponent("dirfleet_trash_\(UUID().uuidString)")
        try FileManager.default.createDirectory(at: folder, withIntermediateDirectories: true)
        defer { try? FileManager.default.removeItem(at: folder) }

        let core = Core(picker: FakePicker(path: nil))
        core.update(.newWindow(path: folder.path))
        try await waitFor("the folder") { core.window(1)?.active.loading == false }
        core.update(.openTrash(tab: 1))
        try await waitFor("the Trash") {
            core.window(1)?.active.inTrash == true && core.window(1)?.active.loading == false
        }
        XCTAssertEqual(core.window(1)?.active.title, core.view.trash.label)
        XCTAssertFalse(core.view.trash.canRestore, "macOS cannot put items back")
        XCTAssertEqual(core.window(1)?.active.nav.up, false)

        core.update(.back(tab: 1))
        try await waitFor("the folder again") { core.window(1)?.active.path == folder.path }
        XCTAssertEqual(core.window(1)?.active.inTrash, false)
    }

    func testSearchFiltersAsYouTypeAndFindsResultsInSubfolders() async throws {
        let folder = FileManager.default.temporaryDirectory
            .appendingPathComponent("dirfleet_search_\(UUID().uuidString)")
        let deep = folder.appendingPathComponent("deep/inner")
        try FileManager.default.createDirectory(at: deep, withIntermediateDirectories: true)
        defer { try? FileManager.default.removeItem(at: folder) }
        for file in [folder.appendingPathComponent("report.pdf"),
                     folder.appendingPathComponent("notes.txt"),
                     deep.appendingPathComponent("old-report.md")] {
            try Data("x".utf8).write(to: file)
        }

        let core = Core(picker: FakePicker(path: nil))
        core.update(.newWindow(path: folder.path))
        try await waitFor("the folder") { core.window(1)?.active.loading == false }
        core.update(.setSearchText(tab: 1, text: "rep"))
        try await waitFor("the filtered folder") {
            core.window(1)?.active.loading == false && core.window(1)?.active.totalRows == 1
        }
        XCTAssertEqual(core.window(1)?.active.rows.first?.highlights, [NameRange(start: 0, end: 3)])

        core.update(.setSearchDepth(tab: 1, depth: 3))
        try await waitFor("the walk through subfolders") {
            core.window(1)?.active.search?.searching == false && core.window(1)?.active.totalRows == 2
        }
        let tab = try XCTUnwrap(core.window(1)?.active)
        XCTAssertTrue(tab.showFolder)
        XCTAssertEqual(Set(tab.rows.map(\.folder)), ["", "deep/inner"])

        core.update(.closeSearch(tab: 1))
        try await waitFor("the whole folder") { core.window(1)?.active.totalRows == 3 }
        XCTAssertNil(core.window(1)?.active.search)
    }

    func testTheSidebarShowsFoldersDrivesFavoritesAndRecentFolders() async throws {
        let dir = FileManager.default.temporaryDirectory
            .appendingPathComponent("dirfleet_sidebar_\(UUID().uuidString)")
        let other = dir.appendingPathComponent("other")
        try FileManager.default.createDirectory(at: other, withIntermediateDirectories: true)
        defer { try? FileManager.default.removeItem(at: dir) }

        let core = Core(picker: FakePicker(path: nil), stateFile: dir.appendingPathComponent("state.json").path)
        core.update(.started(open: dir.path, home: NSHomeDirectory()))
        let sections = { core.view.snapshot.first?.sidebar ?? [] }
        func items(_ section: SidebarSection) -> [SidebarItem] {
            sections().first { $0.section == section }?.items ?? []
        }
        try await waitFor("the user's folders and the drives") {
            !items(.drives).isEmpty && items(.locations).count > 1
        }
        XCTAssertEqual(items(.locations).first?.path, NSHomeDirectory())
        XCTAssertEqual(items(.locations).last?.trash, true)
        XCTAssertTrue(items(.drives).contains { $0.path == "/" }, "the boot volume is listed")

        core.update(.addFavorite(path: dir.path))
        try await waitFor("the favorite") { !items(.favorites).isEmpty }
        XCTAssertEqual(items(.favorites).map(\.path), [dir.path])
        XCTAssertEqual(items(.favorites).first?.active, true)

        let tab = try XCTUnwrap(core.view.snapshot.first?.active.id)
        core.update(.navigate(tab: tab, path: other.path))
        core.update(.navigate(tab: tab, path: dir.path))
        try await waitFor("the recent folder") { !items(.recent).isEmpty }
        XCTAssertEqual(items(.recent).map(\.path), [other.path], "recent leaves out the folder shown")
    }

    func testOpenWithAsksTheShellForAnApp() async throws {
        let folder = FileManager.default.temporaryDirectory
            .appendingPathComponent("dirfleet_openwith_\(UUID().uuidString)")
        try FileManager.default.createDirectory(at: folder, withIntermediateDirectories: true)
        defer { try? FileManager.default.removeItem(at: folder) }
        try Data("x".utf8).write(to: folder.appendingPathComponent("a.txt"))

        let spy = AppPickerSpy()
        let core = Core(picker: spy)
        core.update(.newWindow(path: folder.path))
        try await waitFor("the folder") { core.window(1)?.active.totalRows == 1 }
        core.update(.click(tab: 1, row: 0, command: false, shift: false))
        core.update(.openWith(tab: 1))
        try await waitFor("the app picker") { !spy.asked.isEmpty }
        XCTAssertEqual(spy.asked, ["Open with"])
    }

    /// Real disk changes through the Rust worker. The Trash is left out: on
    /// macOS it asks for permission to drive Finder.
    func testFileOperationsChangeTheDiskAndUndoReverts() async throws {
        let fm = FileManager.default
        let dir = fm.temporaryDirectory.appendingPathComponent("dirfleet_ops_\(UUID().uuidString)")
        let other = dir.appendingPathComponent("other")
        try fm.createDirectory(at: other, withIntermediateDirectories: true)
        defer { try? fm.removeItem(at: dir) }
        try Data("x".utf8).write(to: dir.appendingPathComponent("a.txt"))
        try Data("y".utf8).write(to: other.appendingPathComponent("a.txt"))
        func exists(_ name: String) -> Bool { fm.fileExists(atPath: dir.appendingPathComponent(name).path) }

        let core = Core(picker: FakePicker(path: nil))
        core.update(.newWindow(path: dir.path))
        try await waitFor("the folder") { core.window(1)?.active.totalRows == 2 }

        core.update(.newFolder(tab: 1))
        try await waitFor("the new folder, being renamed") { core.window(1)?.active.renaming != nil }
        XCTAssertTrue(exists("New folder"))
        core.update(.rename(tab: 1, name: "Made"))
        try await waitFor("the rename") { exists("Made") && core.view.undo == "rename" }

        core.update(.undo)
        try await waitFor("the undone rename") { exists("New folder") && !exists("Made") }

        let row = { (name: String) in core.window(1)?.active.rows.first { $0.name == name }?.index }
        try await waitFor("a.txt listed") { row("a.txt") != nil }
        core.update(.click(tab: 1, row: row("a.txt")!, command: false, shift: false))
        core.update(.duplicate(tab: 1))
        try await waitFor("the duplicate") { (try? fm.contentsOfDirectory(atPath: dir.path).count) == 4 }

        core.update(.click(tab: 1, row: row("a.txt")!, command: false, shift: false))
        core.update(.copyTo(tab: 1, folder: other.path))
        try await waitFor("the clash") { core.view.conflict != nil }
        XCTAssertEqual(core.view.conflict?.name, "a.txt")
        core.update(.resolveConflict(choice: .keepBoth))
        try await waitFor("a kept copy") { (try? fm.contentsOfDirectory(atPath: other.path).count) == 2 }
        XCTAssertEqual(try String(contentsOf: other.appendingPathComponent("a.txt"), encoding: .utf8), "y")
    }

    func testThePathBarListsSubfoldersAndGoToFolderChecksTheFolder() async throws {
        let fm = FileManager.default
        let dir = fm.temporaryDirectory.appendingPathComponent("dirfleet_path_\(UUID().uuidString)")
        for name in ["summer", "spring", "winter"] {
            try fm.createDirectory(at: dir.appendingPathComponent(name), withIntermediateDirectories: true)
        }
        defer { try? fm.removeItem(at: dir) }

        let core = Core(picker: FakePicker(path: nil))
        core.update(.newWindow(path: dir.path))
        try await waitFor("the folder") { core.window(1)?.active.totalRows == 3 }
        XCTAssertEqual(core.window(1)?.active.crumbs.last?.title, dir.lastPathComponent)

        core.update(.listChildren(tab: 1, path: dir.path))
        try await waitFor("the subfolders") { core.window(1)?.active.children != nil }
        XCTAssertEqual(core.window(1)?.active.children?.names, ["spring", "summer", "winter"])

        core.update(.openGoTo(tab: 1))
        core.update(.goToTyped(tab: 1, text: dir.path + "/s"))
        try await waitFor("the matches") { core.window(1)?.active.goto?.matches.count == 2 }
        core.update(.goToTyped(tab: 1, text: dir.path + "/autumn"))
        core.update(.goToSubmit(tab: 1, path: nil))
        try await waitFor("the missing folder") { core.window(1)?.active.goto?.error != nil }
        core.update(.goToSubmit(tab: 1, path: dir.appendingPathComponent("winter").path))
        try await waitFor("the folder opened") { core.window(1)?.active.title == "winter" }
        XCTAssertNil(core.window(1)?.active.goto)
    }

    func testAFinishedOperationShowsAToastThatGoesAway() async throws {
        let fm = FileManager.default
        let dir = fm.temporaryDirectory.appendingPathComponent("dirfleet_toast_\(UUID().uuidString)")
        try fm.createDirectory(at: dir, withIntermediateDirectories: true)
        defer { try? fm.removeItem(at: dir) }
        try Data("x".utf8).write(to: dir.appendingPathComponent("a.txt"))

        let core = Core(picker: FakePicker(path: nil))
        core.update(.newWindow(path: dir.path))
        try await waitFor("the folder") { core.window(1)?.active.totalRows == 1 }
        core.update(.click(tab: 1, row: 0, command: false, shift: false))
        core.update(.duplicate(tab: 1))
        try await waitFor("the toast") { !core.view.toasts.isEmpty }
        XCTAssertEqual(core.view.toasts.first?.text, "Duplicated 1 item")
        XCTAssertEqual(core.view.toasts.first?.kind, .success)
        try await waitFor("the toast to expire") { core.view.toasts.isEmpty }
    }

    func testTheInspectorDescribesTheSelectionAndAddsUpAFolder() async throws {
        let fm = FileManager.default
        let dir = fm.temporaryDirectory.appendingPathComponent("dirfleet_inspect_\(UUID().uuidString)")
        let sub = dir.appendingPathComponent("sub")
        try fm.createDirectory(at: sub, withIntermediateDirectories: true)
        defer { try? fm.removeItem(at: dir) }
        try Data(repeating: 1, count: 5000).write(to: sub.appendingPathComponent("big.bin"))
        try Data("abc".utf8).write(to: dir.appendingPathComponent("a.txt"))

        let core = Core(picker: FakePicker(path: nil))
        core.update(.newWindow(path: dir.path))
        try await waitFor("the folder") { core.window(1)?.active.totalRows == 2 }
        core.update(.setInspector(shown: true))
        core.update(.click(tab: 1, row: 1, command: false, shift: false))
        try await waitFor("the file described") { core.window(1)?.active.inspector?.item?.name == "a.txt" }
        XCTAssertEqual(core.window(1)?.active.inspector?.item?.size, .known("3 B"))

        core.update(.click(tab: 1, row: 0, command: false, shift: false))
        try await waitFor("the folder described") { core.window(1)?.active.inspector?.item?.isDir == true }
        core.update(.calculateSize(path: sub.path))
        try await waitFor("the folder's size") {
            if case .known = core.window(1)?.active.inspector?.item?.size { return true }
            return false
        }

        core.update(.selectAll(tab: 1))
        try await waitFor("the summary") { core.window(1)?.active.inspector?.summary?.count == 2 }
    }

    func testImagesGetThumbnailsInRowsTilesAndTheInspector() async throws {
        let fm = FileManager.default
        let dir = fm.temporaryDirectory.appendingPathComponent("dirfleet_thumbs_\(UUID().uuidString)")
        try fm.createDirectory(at: dir, withIntermediateDirectories: true)
        defer { try? fm.removeItem(at: dir) }
        let wide = NSBitmapImageRep(
            bitmapDataPlanes: nil, pixelsWide: 400, pixelsHigh: 200, bitsPerSample: 8,
            samplesPerPixel: 4, hasAlpha: true, isPlanar: false, colorSpaceName: .deviceRGB,
            bytesPerRow: 0, bitsPerPixel: 0
        )!
        for x in 0 ..< 400 { for y in 0 ..< 200 { wide.setColor(.systemRed, atX: x, y: y) } }
        try wide.representation(using: .png, properties: [:])!.write(to: dir.appendingPathComponent("wide.png"))
        try Data("abc".utf8).write(to: dir.appendingPathComponent("notes.txt"))

        let core = Core(picker: FakePicker(path: nil))
        core.update(.newWindow(path: dir.path))
        let row = { (name: String) in core.window(1)?.active.rows.first { $0.name == name } }
        try await waitFor("the list thumbnail") { row("wide.png")?.thumbnail.isEmpty == false }
        let listFile = try XCTUnwrap(row("wide.png")?.thumbnail)
        let listImage = try XCTUnwrap(NSBitmapImageRep(data: Data(contentsOf: URL(fileURLWithPath: listFile))))
        XCTAssertEqual([listImage.pixelsWide, listImage.pixelsHigh], [64, 32])
        XCTAssertEqual(row("notes.txt")?.thumbnail, "", "only images and videos")

        core.update(.setDisplay(tab: 1, display: .icons))
        try await waitFor("the tile thumbnail") {
            guard let file = row("wide.png")?.thumbnail, !file.isEmpty else { return false }
            return file != listFile
        }
        let tile = try XCTUnwrap(NSImage(contentsOfFile: row("wide.png")!.thumbnail))
        XCTAssertGreaterThan(tile.size.width, 64)

        core.update(.setInspector(shown: true))
        let wideRow = try XCTUnwrap(row("wide.png")?.index)
        core.update(.click(tab: 1, row: wideRow, command: false, shift: false))
        try await waitFor("the preview") {
            if case .ready = core.window(1)?.active.inspector?.item?.preview { return true }
            return false
        }
        let notesRow = try XCTUnwrap(row("notes.txt")?.index)
        core.update(.click(tab: 1, row: notesRow, command: false, shift: false))
        try await waitFor("no preview for text") { core.window(1)?.active.inspector?.item?.preview == PreviewState.none }
    }

    func testAGifTilePlaysOnHoverAndWhenSelected() async throws {
        let fm = FileManager.default
        let dir = fm.temporaryDirectory.appendingPathComponent("dirfleet_play_\(UUID().uuidString)")
        try fm.createDirectory(at: dir, withIntermediateDirectories: true)
        defer { try? fm.removeItem(at: dir) }
        // A two-frame GIF, red then blue.
        let gif = dir.appendingPathComponent("loop.gif")
        let destination = try XCTUnwrap(CGImageDestinationCreateWithURL(gif as CFURL, "com.compuserve.gif" as CFString, 2, nil))
        for color in [NSColor.systemRed, NSColor.systemBlue] {
            let frame = NSBitmapImageRep(
                bitmapDataPlanes: nil, pixelsWide: 200, pixelsHigh: 100, bitsPerSample: 8,
                samplesPerPixel: 4, hasAlpha: true, isPlanar: false, colorSpaceName: .deviceRGB,
                bytesPerRow: 0, bitsPerPixel: 0
            )!
            for x in 0 ..< 200 { for y in 0 ..< 100 { frame.setColor(color, atX: x, y: y) } }
            let delay = [kCGImagePropertyGIFDictionary: [kCGImagePropertyGIFDelayTime: 0.1]]
            CGImageDestinationAddImage(destination, try XCTUnwrap(frame.cgImage), delay as CFDictionary)
        }
        XCTAssertTrue(CGImageDestinationFinalize(destination))

        let core = Core(picker: FakePicker(path: nil))
        core.update(.newWindow(path: dir.path))
        let row = { core.window(1)?.active.rows.first { $0.name == "loop.gif" } }
        try await waitFor("the folder") { row() != nil }
        core.update(.setDisplay(tab: 1, display: .icons))
        core.update(.hoverRow(tab: 1, row: 0, inside: true))
        try await waitFor("the tile playing") { row()?.animation.isEmpty == false }
        let file = try XCTUnwrap(row()?.animation)
        let source = try XCTUnwrap(CGImageSourceCreateWithURL(URL(fileURLWithPath: file) as CFURL, nil))
        XCTAssertEqual(CGImageSourceGetCount(source), 2, "both frames play")

        core.update(.hoverRow(tab: 1, row: 0, inside: false))
        try await waitFor("the tile stopping") { row()?.animation == "" }

        var settings = core.view.settings
        settings.animateSelectedPreviews = true
        core.update(.changeSettings(settings: settings))
        core.update(.click(tab: 1, row: 0, command: false, shift: false))
        try await waitFor("the selected tile playing") { row()?.animation == file }
    }

    func testListingDetailsDatesPermissionsAndPackages() async throws {
        let fm = FileManager.default
        let dir = fm.temporaryDirectory.appendingPathComponent("dirfleet_details_\(UUID().uuidString)")
        let locked = dir.appendingPathComponent("locked")
        let app = dir.appendingPathComponent("Tool.app/Contents")
        try fm.createDirectory(at: locked, withIntermediateDirectories: true)
        try fm.createDirectory(at: app, withIntermediateDirectories: true)
        try Data("<plist/>".utf8).write(to: app.appendingPathComponent("Info.plist"))
        try Data(repeating: 1, count: 2500).write(to: dir.appendingPathComponent("notes.txt"))
        defer {
            try? fm.setAttributes([.posixPermissions: 0o755], ofItemAtPath: locked.path)
            try? fm.removeItem(at: dir)
        }

        // Started, the core gets the time and dates read as time ago.
        let core = Core(picker: FakePicker(path: nil))
        var window: UInt32?
        core.start(open: dir.path, home: dir.path) { window = $0 }
        let row = { (name: String) in core.window(window)?.active.rows.first { $0.name == name } }
        try await waitFor("the folder") { row("notes.txt") != nil }
        let modified = try XCTUnwrap(core.view.columns.dropFirst().firstIndex { $0.column == .modified }) - 1
        try await waitFor("a date as time ago") { row("notes.txt")?.cells[modified] == "Just now" }
        XCTAssertTrue(row("notes.txt")?.tooltip.hasPrefix("Modified: ") == true, "the exact date in the tooltip")
        XCTAssertEqual(row("notes.txt")?.detail, "2.5 KB")

        // An app bundle is one item, not a folder to open.
        XCTAssertEqual(row("Tool.app")?.isDir, false)
        XCTAssertEqual(core.window(window)?.active.totalRows, 3)

        // A folder the app may not read offers the Privacy settings.
        try fm.setAttributes([.posixPermissions: 0], ofItemAtPath: locked.path)
        let tab = try XCTUnwrap(core.window(window)?.active.id)
        core.update(.navigate(tab: tab, path: locked.path))
        try await waitFor("the refusal") { core.window(window)?.active.denied == true }
        XCTAssertTrue(core.window(window)?.active.error?.contains("locked") == true)
    }

    func testFileOperationsRenameManyMoveToDeleteAndListActivity() async throws {
        let fm = FileManager.default
        let dir = fm.temporaryDirectory.appendingPathComponent("dirfleet_ops_\(UUID().uuidString)")
        let other = dir.appendingPathComponent("other")
        try fm.createDirectory(at: other, withIntermediateDirectories: true)
        try fm.createDirectory(at: dir.appendingPathComponent("Tool.app/Contents"), withIntermediateDirectories: true)
        defer { try? fm.removeItem(at: dir) }
        for name in ["a.jpg", "b.jpg"] {
            try Data("x".utf8).write(to: dir.appendingPathComponent(name))
        }
        let exists = { (path: String) in fm.fileExists(atPath: dir.appendingPathComponent(path).path) }

        let core = Core(picker: FakePicker(path: nil))
        core.update(.newWindow(path: dir.path))
        let tab = { core.window(1)?.active }
        let index = { (name: String) in tab()?.rows.first { $0.name == name }?.index }
        try await waitFor("the folder") { tab()?.totalRows == 4 }

        // A new text document is made and renamed in place.
        core.update(.newTextFile(tab: 1))
        try await waitFor("the new document renaming") { tab()?.renaming != nil }
        core.update(.rename(tab: 1, name: "notes.txt"))
        try await waitFor("the document on disk") { exists("notes.txt") }

        // Two selected items renamed at once, from a pattern.
        try await waitFor("the document listed") { tab()?.totalRows == 5 }
        core.update(.click(tab: 1, row: try XCTUnwrap(index("a.jpg")), command: false, shift: false))
        core.update(.click(tab: 1, row: try XCTUnwrap(index("b.jpg")), command: true, shift: false))
        core.update(.startRename(tab: 1))
        try await waitFor("batch rename") { tab()?.batchRename != nil }
        var fields = try XCTUnwrap(tab()?.batchRename?.fields)
        fields.mode = .pattern
        fields.pattern = "Trip {n}{ext}"
        core.update(.setBatchRename(tab: 1, fields: fields))
        try await waitFor("the preview") { tab()?.batchRename?.items.map(\.new) == ["Trip 1.jpg", "Trip 2.jpg"] }
        core.update(.applyBatchRename(tab: 1))
        try await waitFor("both renamed") { exists("Trip 1.jpg") && exists("Trip 2.jpg") && !exists("a.jpg") }

        // Move To another tab's folder, from the submenu's targets.
        core.update(.newTab(window: 1, path: other.path))
        try await waitFor("the other tab") { tab()?.path == other.path }
        core.update(.activateTab(tab: 1))
        try await waitFor("back in the first tab") { tab()?.id == 1 && index("Trip 1.jpg") != nil }
        let target = try XCTUnwrap(tab()?.transferTargets.first { $0.path == other.path })
        XCTAssertEqual(target.detail, "Tab")
        core.update(.click(tab: 1, row: try XCTUnwrap(index("Trip 1.jpg")), command: false, shift: false))
        core.update(.moveTo(tab: 1, folder: target.path))
        try await waitFor("the move") { fm.fileExists(atPath: other.appendingPathComponent("Trip 1.jpg").path) }

        // Delete permanently (the shell has already confirmed).
        try await waitFor("the move listed") { index("Trip 1.jpg") == nil && index("Trip 2.jpg") != nil }
        core.update(.click(tab: 1, row: try XCTUnwrap(index("Trip 2.jpg")), command: false, shift: false))
        core.update(.deleteSelection(tab: 1))
        try await waitFor("the deletion") { !exists("Trip 2.jpg") }

        // Each finished operation is in the Activity list, newest first.
        try await waitFor("the activity") { core.view.activity.count >= 4 }
        XCTAssertTrue(core.view.activity.allSatisfy { $0.state == .done })

        // An app bundle opens as a folder from Show Package Contents.
        try await waitFor("the bundle listed") { index("Tool.app") != nil }
        core.update(.click(tab: 1, row: try XCTUnwrap(index("Tool.app")), command: false, shift: false))
        core.update(.showPackageContents(tab: 1))
        try await waitFor("inside the bundle") { tab()?.path.hasSuffix("Tool.app") == true }
    }

    func testAccessTerminalAndAboutAfterStart() async throws {
        let dir = FileManager.default.temporaryDirectory.appendingPathComponent("dirfleet_access_\(UUID().uuidString)")
        try FileManager.default.createDirectory(at: dir, withIntermediateDirectories: true)
        defer { try? FileManager.default.removeItem(at: dir) }
        let core = Core(picker: FakePicker(path: nil))
        var window: UInt32?
        core.start(open: dir.path, home: dir.path) { window = $0 }
        try await waitFor("the folder") { core.window(window)?.active.loading == false }

        // The test runner is not a built app: no Full Disk Access banner.
        try await waitFor("the terminal check") { core.view.access.terminalFound }
        XCTAssertEqual(core.view.access.prompt, .hidden)
        XCTAssertFalse(core.view.access.diskAccessApplies)
        XCTAssertTrue(core.view.access.version.hasPrefix("Version "))

        var settings = core.view.settings
        settings.terminal = "Definitely Not A Terminal 42"
        core.update(.changeSettings(settings: settings))
        try await waitFor("the missing terminal noticed") { !core.view.access.terminalFound }
        settings.terminal = "Terminal"
        core.update(.changeSettings(settings: settings))
        try await waitFor("Terminal found again") { core.view.access.terminalFound }
    }

    func testSearchUndoNoResultsOffersAndEscape() async throws {
        let fm = FileManager.default
        let dir = fm.temporaryDirectory.appendingPathComponent("dirfleet_search2_\(UUID().uuidString)")
        let deep = dir.appendingPathComponent("a/b")
        try fm.createDirectory(at: deep, withIntermediateDirectories: true)
        defer { try? fm.removeItem(at: dir) }
        try Data("x".utf8).write(to: deep.appendingPathComponent("hidden-gem.txt"))
        try Data("x".utf8).write(to: dir.appendingPathComponent("notes.txt"))

        let core = Core(picker: FakePicker(path: nil))
        core.update(.newWindow(path: dir.path))
        let tab = { core.window(1)?.active }
        try await waitFor("the folder") { tab()?.totalRows == 2 }

        // Nothing here: the bar offers subfolders, which find it.
        core.update(.setSearchText(tab: 1, text: "gem"))
        try await waitFor("the offer") { tab()?.search?.offerSubfolders == true && tab()?.totalRows == 0 }
        XCTAssertFalse(core.view.searchHelp.isEmpty)
        core.update(.setSearchDepth(tab: 1, depth: 4))
        try await waitFor("the deep result") { tab()?.rows.first?.name == "hidden-gem.txt" }
        XCTAssertEqual(tab()?.search?.offerSubfolders, false)

        // Search edits are on the Edit menu's Undo.
        XCTAssertEqual(core.view.undo, "search subfolders")
        core.update(.undo)
        try await waitFor("the depth undone") { tab()?.search?.depth == 1 }
        XCTAssertEqual(core.view.redo, "search subfolders")

        // Escape: the text goes, then the search closes.
        core.update(.searchEscape(tab: 1))
        try await waitFor("the text cleared") { tab()?.search?.text == "" }
        core.update(.searchEscape(tab: 1))
        try await waitFor("the search closed") { tab()?.search == nil && tab()?.totalRows == 2 }
    }

    func testFramesSectionOrderAndFolderViewsComeBackAndSavedSearchesOpenInNewTabs() async throws {
        let fm = FileManager.default
        let dir = fm.temporaryDirectory.appendingPathComponent("dirfleet_persist_\(UUID().uuidString)")
        let folder = dir.appendingPathComponent("home")
        try fm.createDirectory(at: folder, withIntermediateDirectories: true)
        defer { try? fm.removeItem(at: dir) }
        try Data("x".utf8).write(to: folder.appendingPathComponent("report.txt"))
        let stateFile = dir.appendingPathComponent("state.json").path

        let first = Core(picker: FakePicker(path: nil), stateFile: stateFile)
        var window: UInt32?
        first.start(open: nil, home: folder.path) { window = $0 }
        try await waitFor("home") { first.window(window)?.active.totalRows == 1 }
        let id = try XCTUnwrap(window)
        let tab = try XCTUnwrap(first.window(window)?.active.id)
        let frame = WindowFrame(x: 120, y: 80, width: 820, height: 540)
        first.update(.windowFramed(window: id, frame: frame))
        first.update(.addFavorite(path: folder.path))
        first.update(.setDisplay(tab: tab, display: .icons))
        try await waitFor("favorites shown") { first.window(window)?.sidebar.first?.section == .favorites }
        let last = try XCTUnwrap(first.window(window)?.sidebar.last?.section)
        first.update(.moveSidebarSection(section: last, to: .favorites))
        try await waitFor("the order changed") { first.window(window)?.sidebar.first?.section == last }

        // A saved search opens in a new window (a native tab in the app).
        first.update(.setSearchText(tab: tab, text: "report"))
        first.update(.saveSearch(tab: tab, name: "Reports"))
        try await waitFor("saved") { !first.view.savedSearches.isEmpty }
        first.update(.closeSearch(tab: tab))
        first.update(.runSavedSearchInNewWindow(tab: tab, id: first.view.savedSearches[0].id))
        try await waitFor("the new window") {
            first.view.snapshot.count == 2 && first.view.snapshot[1].active.saved?.name == "Reports"
        }
        XCTAssertEqual(first.view.snapshot[1].active.path, folder.path)
        try await waitFor("the state saved") {
            (try? String(contentsOfFile: stateFile, encoding: .utf8))?.contains("820") == true
        }

        let second = Core(picker: FakePicker(path: nil), stateFile: stateFile)
        var again: UInt32?
        second.start(open: nil, home: folder.path) { again = $0 }
        try await waitFor("restored") { second.window(again)?.active.totalRows == 1 }
        XCTAssertEqual(second.window(again)?.frame, frame)
        XCTAssertEqual(second.window(again)?.active.view.display, .icons, "the folder's own view")
        try await waitFor("the moved section first again") { second.window(again)?.sidebar.first?.section == last }
    }

    func testTheStatusLineShowsFreeSpaceAndARightClickSelectsItsRow() async throws {
        let fm = FileManager.default
        let dir = fm.temporaryDirectory.appendingPathComponent("dirfleet_status_\(UUID().uuidString)")
        try fm.createDirectory(at: dir, withIntermediateDirectories: true)
        defer { try? fm.removeItem(at: dir) }
        for name in ["a.txt", "b.txt"] {
            try Data("x".utf8).write(to: dir.appendingPathComponent(name))
        }
        let core = Core(picker: FakePicker(path: nil))
        var window: UInt32?
        core.start(open: dir.path, home: dir.path) { window = $0 }
        let tab = { core.window(window)?.active }
        try await waitFor("the folder") { tab()?.totalRows == 2 }

        // The drives are listed: the folder's drive says how much room it has.
        try await waitFor("the drive's free space") { tab()?.driveStatus?.free.hasSuffix("available") == true }
        XCTAssertTrue(tab()?.driveStatus?.detail.contains("free of") == true)
        XCTAssertNil(tab()?.driveStatus?.eject, "the system disk isn't ejected")

        let id = try XCTUnwrap(tab()?.id)
        core.update(.click(tab: id, row: 0, command: false, shift: false))
        core.update(.contextClick(tab: id, row: 1))
        try await waitFor("the right-clicked row selected") {
            tab()?.rows.filter(\.selected).map(\.name) == ["b.txt"]
        }
    }

    /// Reading a file updates when it was last opened, which no folder
    /// watcher reports; coming back to the window reads the folder again.
    func testComingBackToTheWindowReordersAFolderSortedByDateOpened() async throws {
        let fm = FileManager.default
        let dir = fm.temporaryDirectory.appendingPathComponent("dirfleet_focus_\(UUID().uuidString)")
        try fm.createDirectory(at: dir, withIntermediateDirectories: true)
        defer { try? fm.removeItem(at: dir) }
        // Opened at the start of 2001 and of 2000, changed in 2002: APFS
        // only notes a read of a file opened before its last change.
        for (name, opened) in [("a.txt", 978_307_200), ("b.txt", 946_684_800)] {
            let file = dir.appendingPathComponent(name)
            try Data("x".utf8).write(to: file)
            var times = [timeval(tv_sec: opened, tv_usec: 0), timeval(tv_sec: 1_009_843_200, tv_usec: 0)]
            XCTAssertEqual(utimes(file.path, &times), 0)
        }
        let core = Core(picker: FakePicker(path: nil))
        var window: UInt32?
        core.start(open: dir.path, home: dir.path) { window = $0 }
        let tab = { core.window(window)?.active }
        try await waitFor("the folder") { tab()?.totalRows == 2 }
        let id = try XCTUnwrap(tab()?.id)
        core.update(.sortBy(tab: id, sort: .accessed))
        try await waitFor("sorted by date opened") { tab()?.view.sort == .accessed && tab()?.loading == false }
        // A new folder shows up once the folder is watched (folders come first).
        try fm.createDirectory(at: dir.appendingPathComponent("c"), withIntermediateDirectories: false)
        try await waitFor("the watch running") { tab()?.totalRows == 3 && tab()?.loading == false }
        let files = { tab()?.rows.map(\.name).filter { $0.hasSuffix(".txt") } }
        let before = try XCTUnwrap(files())

        // Open the one opened longest ago, as another app would.
        _ = try Data(contentsOf: dir.appendingPathComponent("b.txt"))
        try await Task.sleep(for: .milliseconds(300))
        XCTAssertEqual(files(), before, "nothing tells the app yet")

        let nsWindow = NSWindow(contentRect: .init(x: 0, y: 0, width: 400, height: 300),
                                styleMask: [.titled], backing: .buffered, defer: true)
        core.register(window: try XCTUnwrap(window), nsWindow: nsWindow)
        NotificationCenter.default.post(name: NSWindow.didBecomeKeyNotification, object: nsWindow)
        try await waitFor("read again, in the new order") { files() == before.reversed() }
    }

    /// Files opened earlier (the history beside the state file) show when
    /// and how often; a file is forgotten, then the whole history, on disk too.
    func testOpenHistoryShowsInColumnsAndIsForgottenAndCleared() async throws {
        let fm = FileManager.default
        let dir = fm.temporaryDirectory.appendingPathComponent("dirfleet_opened_\(UUID().uuidString)")
        let folder = dir.appendingPathComponent("files")
        try fm.createDirectory(at: folder, withIntermediateDirectories: true)
        defer { try? fm.removeItem(at: dir) }
        for name in ["a.txt", "b.txt"] {
            try Data("x".utf8).write(to: folder.appendingPathComponent(name))
        }
        let history = dir.appendingPathComponent("open_history.json")
        let opened = [
            folder.appendingPathComponent("a.txt").path: ["last_opened": 1_762_610_580, "count": 3],
            folder.appendingPathComponent("b.txt").path: ["last_opened": 1_762_610_000, "count": 2],
        ]
        try JSONSerialization.data(withJSONObject: opened).write(to: history)

        let core = Core(picker: FakePicker(path: nil), stateFile: dir.appendingPathComponent("state.json").path)
        var window: UInt32?
        core.start(open: folder.path, home: folder.path) { window = $0 }
        let tab = { core.window(window)?.active }
        try await waitFor("the folder") { tab()?.totalRows == 2 && tab()?.loading == false }
        if !core.view.columns.contains(where: { $0.column == .openCount }) {
            core.update(.toggleColumn(column: .openCount))
        }
        // Cells follow the shown columns after Name.
        let opens = {
            core.view.columns.dropFirst().firstIndex { $0.column == .openCount }.map { index in
                tab()?.rows.map { $0.cells[index - 1] } ?? []
            } ?? []
        }
        try await waitFor("the counts") { opens() == ["3", "2"] }

        let id = try XCTUnwrap(tab()?.id)
        core.update(.click(tab: id, row: 0, command: false, shift: false))
        core.update(.forgetOpened(tab: id))
        try await waitFor("a.txt forgotten") { opens() == ["", "2"] }
        let kept = try XCTUnwrap(JSONSerialization.jsonObject(with: Data(contentsOf: history)) as? [String: Any])
        XCTAssertEqual(kept.keys.map { URL(fileURLWithPath: $0).lastPathComponent }, ["b.txt"])

        core.update(.clearOpenHistory)
        try await waitFor("everything forgotten") { opens() == ["", ""] }
        let left = try XCTUnwrap(JSONSerialization.jsonObject(with: Data(contentsOf: history)) as? [String: Any])
        XCTAssertTrue(left.isEmpty)

        var settings = core.view.settings
        settings.trackOpenHistory = false
        core.update(.changeSettings(settings: settings))
        try await waitFor("opens no longer remembered") { core.view.settings.trackOpenHistory == false }
    }

    /// A real video's frames are offered, one becomes its thumbnail (kept
    /// beside the state file), and resetting it, or all, brings the default back.
    /// Needs FFmpeg, as the shared media code does; skipped without it.
    func testAVideosThumbnailIsChosenFromItsFramesAndReset() async throws {
        let ffmpeg = ["/opt/homebrew/bin/ffmpeg", "/usr/local/bin/ffmpeg"]
            .first { FileManager.default.isExecutableFile(atPath: $0) }
        guard let tool = ffmpeg else { throw XCTSkip("FFmpeg isn't installed") }
        let fm = FileManager.default
        let dir = fm.temporaryDirectory.appendingPathComponent("dirfleet_picker_\(UUID().uuidString)")
        let folder = dir.appendingPathComponent("videos")
        try fm.createDirectory(at: folder, withIntermediateDirectories: true)
        defer { try? fm.removeItem(at: dir) }
        let clip = folder.appendingPathComponent("clip.mp4")
        let make = Process()
        make.executableURL = URL(fileURLWithPath: tool)
        make.arguments = ["-loglevel", "error", "-f", "lavfi", "-i", "testsrc=duration=6:size=320x180:rate=10",
                          "-pix_fmt", "yuv420p", clip.path]
        try make.run()
        make.waitUntilExit()
        XCTAssertEqual(make.terminationStatus, 0)
        let choices = dir.appendingPathComponent("thumbnail_choices.json")

        let core = Core(picker: FakePicker(path: nil), stateFile: dir.appendingPathComponent("state.json").path)
        var window: UInt32?
        core.start(open: folder.path, home: folder.path) { window = $0 }
        let tab = { core.window(window)?.active }
        try await waitFor("the video") { tab()?.rows.first?.thumbnailChoice == .automatic }
        let id = try XCTUnwrap(tab()?.id)

        core.update(.selectThumbnail(tab: id, row: 0))
        let ready = { core.view.thumbnailPicker?.frames.filter { !$0.image.isEmpty } ?? [] }
        try await waitFor("two frames extracted") { ready().count >= 2 }
        XCTAssertEqual(core.view.thumbnailPicker?.duration, "0:06")
        XCTAssertTrue(fm.fileExists(atPath: ready()[0].image), "frames are image files")
        let other = try XCTUnwrap(ready().first { $0.badge != "Default" })
        core.update(.selectPickerFrame(atMs: other.atMs))
        try await waitFor("can use it") { core.view.thumbnailPicker?.canApply == true }
        core.update(.usePickerFrame)
        try await waitFor("chosen") { core.view.thumbnailPicker == nil && tab()?.rows.first?.thumbnailChoice == .chosen }
        let kept = try String(contentsOf: choices, encoding: .utf8)
        XCTAssertTrue(kept.contains("clip.mp4") && kept.contains("\(other.atMs)"), kept)
        try await waitFor("the poster from that frame") { tab()?.rows.first?.thumbnail.contains("_t\(other.atMs)") == true }

        core.update(.resetThumbnail(tab: id, row: 0))
        try await waitFor("the default again") { tab()?.rows.first?.thumbnailChoice == .automatic }
        XCTAssertFalse(try String(contentsOf: choices, encoding: .utf8).contains("clip.mp4"))

        // Chosen again, then Settings' Reset All.
        core.update(.selectThumbnail(tab: id, row: 0))
        try await waitFor("the frames again") { ready().contains { $0.atMs == other.atMs } }
        core.update(.selectPickerFrame(atMs: other.atMs))
        core.update(.usePickerFrame)
        try await waitFor("chosen again") { tab()?.rows.first?.thumbnailChoice == .chosen }
        core.update(.resetAllThumbnails)
        try await waitFor("all reset") { tab()?.rows.first?.thumbnailChoice == .automatic }
        XCTAssertEqual(try String(contentsOf: choices, encoding: .utf8), "{}")
    }

    /// A plugin in the plugins folder beside the state: off until turned on
    /// and its permission allowed, then its action is offered for a file and
    /// runs in Lua.
    func testAUserPluginIsTurnedOnAndItsActionRunsOnTheSelection() async throws {
        let fm = FileManager.default
        let dir = fm.temporaryDirectory.appendingPathComponent("dirfleet_plugins_\(UUID().uuidString)")
        let folder = dir.appendingPathComponent("files")
        let plugin = dir.appendingPathComponent("plugins/io.example.hello")
        try fm.createDirectory(at: folder, withIntermediateDirectories: true)
        try fm.createDirectory(at: plugin, withIntermediateDirectories: true)
        defer { try? fm.removeItem(at: dir) }
        try Data("x".utf8).write(to: folder.appendingPathComponent("a.txt"))
        try Data("""
        api_version = 1
        id = "io.example.hello"
        name = "Hello"
        version = "1.0.0"
        author = "Tests"
        permissions = ["clipboard"]

        [[action]]
        id = "greet"
        title = "Say Hello"
        context_menu = "entry"
        targets = { kind = "files" }
        """.utf8).write(to: plugin.appendingPathComponent("plugin.toml"))
        try Data("""
        local M = { actions = {} }
        function M.actions.greet(ctx)
          dirfleet.notify("Hello " .. ctx.targets[1].name, "success")
        end
        return M
        """.utf8).write(to: plugin.appendingPathComponent("main.lua"))

        let core = Core(picker: FakePicker(path: nil), stateFile: dir.appendingPathComponent("state.json").path)
        var window: UInt32?
        core.start(open: folder.path, home: folder.path) { window = $0 }
        let tab = { core.window(window)?.active }
        let hello = { core.view.plugins.plugins.first { $0.id == "io.example.hello" } }
        try await waitFor("the plugin found") { hello() != nil && tab()?.totalRows == 1 }
        XCTAssertEqual(hello()?.status, "Off", "a user's plugin starts off")
        XCTAssertTrue(core.view.plugins.plugins.contains { $0.bundled }, "the built-in ones are listed too")

        core.update(.setPluginEnabled(id: "io.example.hello", enabled: true))
        try await waitFor("waiting for approval") { hello()?.needsApproval == true }
        XCTAssertEqual(hello()?.permissions, ["Write to the clipboard"])
        core.update(.approvePlugin(id: "io.example.hello"))
        try await waitFor("running") { hello()?.status == "Running" }
        let id = try XCTUnwrap(tab()?.id)
        core.update(.click(tab: id, row: 0, command: false, shift: false))
        try await waitFor("its action offered") { tab()?.pluginActions.map(\.title) == ["Say Hello"] }
        core.runPluginAction(try XCTUnwrap(tab()?.pluginActions.first), tab: id)
        try await waitFor("Lua's toast, named after its plugin") { core.view.toasts.contains { $0.text == "Hello: Hello a.txt" } }
    }

    /// A plugin action's form is filled in, refused by the plugin's own
    /// validate, then runs with the values; a per-item job reports how it went.
    func testAPluginFormIsValidatedByLuaAndAJobRunsPerItem() async throws {
        let fm = FileManager.default
        let dir = fm.temporaryDirectory.appendingPathComponent("dirfleet_plugin_form_\(UUID().uuidString)")
        let folder = dir.appendingPathComponent("files")
        let plugin = dir.appendingPathComponent("plugins/io.example.forms")
        try fm.createDirectory(at: folder, withIntermediateDirectories: true)
        try fm.createDirectory(at: plugin, withIntermediateDirectories: true)
        defer { try? fm.removeItem(at: dir) }
        for name in ["a.txt", "b.txt"] {
            try Data("x".utf8).write(to: folder.appendingPathComponent(name))
        }
        try Data("""
        api_version = 1
        id = "io.example.forms"
        name = "Forms"
        version = "1.0.0"
        author = "Tests"

        [[action]]
        id = "greet"
        title = "Greet…"
        context_menu = "entry"
        form = "static"

        [[action.field]]
        id = "who"
        type = "text"
        required = true

        [[action]]
        id = "count"
        title = "Count"
        context_menu = "entry"
        mode = "job"
        per_item = true
        """.utf8).write(to: plugin.appendingPathComponent("plugin.toml"))
        try Data("""
        local M = { actions = {} }
        M.actions.greet = {
          validate = function(ctx, values)
            if values.who == "nobody" then return { who = "Pick someone" } end
          end,
          run = function(ctx, values)
            dirfleet.notify("Hi " .. values.who, "success")
          end,
        }
        M.actions.count = {
          run_item = function(ctx, item, values) end,
        }
        return M
        """.utf8).write(to: plugin.appendingPathComponent("main.lua"))

        let core = Core(picker: FakePicker(path: nil), stateFile: dir.appendingPathComponent("state.json").path)
        var window: UInt32?
        core.start(open: folder.path, home: folder.path) { window = $0 }
        let tab = { core.window(window)?.active }
        try await waitFor("the plugin found") { core.view.plugins.plugins.contains { $0.id == "io.example.forms" } }
        core.update(.setPluginEnabled(id: "io.example.forms", enabled: true))
        try await waitFor("running") {
            core.view.plugins.plugins.first { $0.id == "io.example.forms" }?.status == "Running" && tab()?.totalRows == 2
        }
        let id = try XCTUnwrap(tab()?.id)
        core.update(.selectAll(tab: id))
        try await waitFor("its actions") { tab()?.pluginActions.map(\.title) == ["Greet…", "Count"] }

        core.runPluginAction(try XCTUnwrap(tab()?.pluginActions.first), tab: id)
        try await waitFor("the form") { core.view.pluginForm?.fields.first?.id == "who" }
        XCTAssertEqual(core.view.pluginForm?.tab, id)
        core.update(.setPluginFormField(id: "who", text: "nobody"))
        core.update(.submitPluginForm)
        try await waitFor("Lua refuses it") { core.view.pluginForm?.fields.first?.error == "Pick someone" }
        core.update(.setPluginFormField(id: "who", text: "Ada"))
        core.update(.submitPluginForm)
        try await waitFor("it ran") {
            core.view.pluginForm == nil && core.view.toasts.contains { $0.text == "Forms: Hi Ada" }
        }

        core.runPluginAction(try XCTUnwrap(tab()?.pluginActions.last), tab: id)
        try await waitFor("the job's summary") {
            core.view.toasts.contains { $0.text == "Count (2 items): 2 done" } && core.view.pluginJobs.isEmpty
        }
    }

    /// Folder actions are offered in the empty area where the plugin applies;
    /// a results action's job fills its dialog's table, a row's menu action
    /// runs on its folder, and a row opens.
    func testFolderActionsAndAResultsDialogFromARealPlugin() async throws {
        let fm = FileManager.default
        let dir = fm.temporaryDirectory.appendingPathComponent("dirfleet_plugin_results_\(UUID().uuidString)")
        let folder = dir.appendingPathComponent("files")
        let plugin = dir.appendingPathComponent("plugins/io.example.usage")
        try fm.createDirectory(at: folder.appendingPathComponent("sub"), withIntermediateDirectories: true)
        try fm.createDirectory(at: plugin, withIntermediateDirectories: true)
        defer { try? fm.removeItem(at: dir) }
        try Data("""
        api_version = 1
        id = "io.example.usage"
        name = "Usage"
        version = "1.0.0"
        author = "Tests"

        [activation]
        always = true

        [[action]]
        id = "hello"
        title = "Hello Folder"
        targets = { kind = "current_dir" }
        context_menu = "background"

        [[action]]
        id = "scan"
        title = "Folder Usage…"
        targets = { kind = "current_dir" }
        context_menu = "background"
        mode = "job"
        results = true

        [[action]]
        id = "forget"
        title = "Forget"
        targets = { kind = "current_dir" }
        mode = "job"
        """.utf8).write(to: plugin.appendingPathComponent("plugin.toml"))
        try Data("""
        local M = { actions = {} }
        function M.actions.hello(ctx)
          dirfleet.notify("Hello " .. dirfleet.path.name(ctx.dir), "info")
        end
        M.actions.scan = {
          run = function(ctx, values)
            dirfleet.app.show_table({
              title = "Folder Usage",
              columns = { { id = "name", label = "Folder" }, { id = "size", label = "Size", kind = "size" } },
              rows = {
                { path = ctx.dir .. "/sub", cells = { name = "sub", size = 2048 } },
                { path = ctx.dir .. "/old", cells = { name = "old", size = 1024 },
                  menu = { { label = "Forget", action = "forget", busy = "Forgetting…" } } },
              },
            })
          end,
        }
        M.actions.forget = {
          run = function(ctx, values)
            dirfleet.app.update_table_row({ path = ctx.dir, remove = true })
          end,
        }
        return M
        """.utf8).write(to: plugin.appendingPathComponent("main.lua"))

        let core = Core(picker: FakePicker(path: nil), stateFile: dir.appendingPathComponent("state.json").path)
        var window: UInt32?
        core.start(open: folder.path, home: folder.path) { window = $0 }
        let tab = { core.window(window)?.active }
        try await waitFor("the plugin found") { core.view.plugins.plugins.contains { $0.id == "io.example.usage" } }
        core.update(.setPluginEnabled(id: "io.example.usage", enabled: true))
        // The built-in Folder Usage applies everywhere too: just this plugin's.
        let mine = { tab()?.folderPluginActions.filter { $0.plugin == "Usage" } ?? [] }
        try await waitFor("its folder actions") { mine().map(\.title) == ["Hello Folder", "Folder Usage…"] }
        let id = try XCTUnwrap(tab()?.id)
        let actions = mine()
        core.runPluginAction(actions[0], tab: id)
        try await waitFor("Lua's toast") { core.view.toasts.contains { $0.text == "Usage: Hello files" } }

        core.runPluginAction(actions[1], tab: id)
        try await waitFor("the dialog") { core.view.pluginResults?.title == "Folder Usage: files" }
        XCTAssertEqual(core.view.pluginResults?.tab, id)
        core.update(.runPluginResults)
        try await waitFor("the table") {
            core.view.pluginResults?.rows.first?.cells.first == "sub" && core.view.pluginResults?.running == false
        }
        XCTAssertEqual(core.view.pluginResults?.columns.map(\.title), ["Folder", "Size"])
        // A row's own action runs on its folder; this one drops the row.
        let old = try XCTUnwrap(core.view.pluginResults?.rows.first { $0.cells.first == "old" })
        XCTAssertEqual(old.menu.map(\.label), ["Forget"])
        core.update(.pluginResultsRowAction(path: old.path, action: "forget"))
        try await waitFor("the row gone") { core.view.pluginResults?.rows.map { $0.cells[0] } == ["sub"] }
        let row = try XCTUnwrap(core.view.pluginResults?.rows.first)
        XCTAssertTrue(row.path.hasSuffix("/files/sub"))
        let windows = core.view.snapshot.count
        core.update(.openPluginResultsRow(path: row.path))
        try await waitFor("a new tab on it") {
            core.view.snapshot.count == windows + 1 && core.view.pluginResults == nil
        }
    }

    /// A plugin's bar below the folder, its inspector section for the
    /// selection, an event it subscribes to, and its action in the palette.
    func testAPluginsBarInspectorEventsAndPaletteAction() async throws {
        let fm = FileManager.default
        let dir = fm.temporaryDirectory.appendingPathComponent("dirfleet_plugin_bar_\(UUID().uuidString)")
        let folder = dir.appendingPathComponent("files")
        let plugin = dir.appendingPathComponent("plugins/io.example.status")
        try fm.createDirectory(at: folder.appendingPathComponent("sub"), withIntermediateDirectories: true)
        try fm.createDirectory(at: plugin, withIntermediateDirectories: true)
        defer { try? fm.removeItem(at: dir) }
        try Data("x".utf8).write(to: folder.appendingPathComponent("a.txt"))
        try Data("""
        api_version = 1
        id = "io.example.status"
        name = "Status"
        version = "1.0.0"
        author = "Tests"

        [activation]
        always = true

        [bar]

        [inspector]
        title = "Status"
        targets = { kind = "files", max = 1 }

        [events]
        subscribe = ["folder_opened"]

        [[action]]
        id = "hi"
        title = "Say Hi"
        targets = { kind = "current_dir" }
        """.utf8).write(to: plugin.appendingPathComponent("plugin.toml"))
        try Data("""
        local M = { actions = {} }
        function M.bar(ctx) return { { text = "in " .. dirfleet.path.name(ctx.dir) }, { button = "Hi", action = "hi" } } end
        function M.inspector(ctx) return { { "Name", ctx.targets[1].name } } end
        function M.on_folder_opened(ctx, data) dirfleet.notify("opened " .. dirfleet.path.name(ctx.dir)) end
        function M.actions.hi(ctx) dirfleet.notify("hi") end
        return M
        """.utf8).write(to: plugin.appendingPathComponent("main.lua"))

        let core = Core(picker: FakePicker(path: nil), stateFile: dir.appendingPathComponent("state.json").path)
        var window: UInt32?
        core.start(open: folder.path, home: folder.path) { window = $0 }
        let tab = { core.window(window)?.active }
        try await waitFor("the plugin found") { core.view.plugins.plugins.contains { $0.id == "io.example.status" } }
        core.update(.setPluginEnabled(id: "io.example.status", enabled: true))
        try await waitFor("its bar") { tab()?.pluginBar.first?.segments.first?.text == "in files" }
        XCTAssertEqual(tab()?.pluginBar.first?.segments.last?.action, "io.example.status.hi")

        let id = try XCTUnwrap(tab()?.id)
        core.update(.setInspector(shown: true))
        let file = try XCTUnwrap(tab()?.rows.firstIndex { $0.name == "a.txt" })
        core.update(.click(tab: id, row: UInt32(file), command: false, shift: false))
        try await waitFor("its section") { tab()?.pluginSections.first?.rows.first?.value == "a.txt" }
        XCTAssertEqual(tab()?.pluginSections.first?.title, "Status")

        core.update(.navigate(tab: id, path: folder.appendingPathComponent("sub").path))
        try await waitFor("folder_opened heard") { core.view.toasts.contains { $0.text == "Status: opened sub" } }
        try await waitFor("its bar for the new folder") { tab()?.pluginBar.first?.segments.first?.text == "in sub" }

        core.update(.setPaletteText(text: "Say Hi"))
        let hi = { core.view.palette?.items.first { $0.target == .pluginAction("io.example.status.hi") } }
        try await waitFor("its action in the palette") { hi() != nil }
        let item = try XCTUnwrap(hi())
        core.run(item.target, tab: id, window: try XCTUnwrap(window))
        try await waitFor("the action ran") { core.view.toasts.contains { $0.text == "Status: hi" } }
    }

    func testPluginShortcutsTheSandboxAndAnEditedPluginReloading() async throws {
        let fm = FileManager.default
        let dir = fm.temporaryDirectory.appendingPathComponent("dirfleet_plugin_keys_\(UUID().uuidString)")
        let folder = dir.appendingPathComponent("files")
        let plugin = dir.appendingPathComponent("plugins/io.example.keys")
        try fm.createDirectory(at: folder, withIntermediateDirectories: true)
        try fm.createDirectory(at: plugin, withIntermediateDirectories: true)
        defer { try? fm.removeItem(at: dir) }
        try Data("x".utf8).write(to: folder.appendingPathComponent("a.txt"))
        let manifest = { (title: String) in
            """
            api_version = 1
            id = "io.example.keys"
            name = "Keys"
            version = "1.0.0"
            author = "Tests"

            [[action]]
            id = "spin"
            title = "\(title)"
            shortcut = "Ctrl+Alt+K"
            targets = { kind = "current_dir" }

            [[action]]
            id = "peek"
            title = "Peek"
            targets = { kind = "current_dir" }
            """
        }
        try Data(manifest("Spin").utf8).write(to: plugin.appendingPathComponent("plugin.toml"))
        try Data("""
        local M = { actions = {} }
        function M.actions.spin(ctx) while true do end end
        function M.actions.peek(ctx) dirfleet.fs.read_text(ctx.dir .. "/a.txt") end
        return M
        """.utf8).write(to: plugin.appendingPathComponent("main.lua"))

        let core = Core(picker: FakePicker(path: nil), stateFile: dir.appendingPathComponent("state.json").path)
        var window: UInt32?
        core.start(open: folder.path, home: folder.path) { window = $0 }
        try await waitFor("the plugin found") { core.view.plugins.plugins.contains { $0.id == "io.example.keys" } }
        core.update(.setPluginEnabled(id: "io.example.keys", enabled: true))
        let spin = { core.view.pluginShortcuts.first { $0.action == "io.example.keys.spin" } }
        try await waitFor("its shortcut bound") { spin() != nil }
        let binding = try XCTUnwrap(spin())
        XCTAssertEqual(binding.text, "⌥⌘K")
        XCTAssertEqual(binding.menuKey, "k")
        XCTAssertEqual([binding.command, binding.alt, binding.shift], [true, true, false])

        core.update(.setPluginShortcut(plugin: "io.example.keys", action: "peek", text: "Ctrl+R"))
        let peekSetting = { core.view.plugins.plugins.first { $0.id == "io.example.keys" }?.shortcuts.last }
        try await waitFor("the clash noted") { peekSetting()?.note == "⌘R is used by “Refresh”" }
        core.update(.setPluginShortcut(plugin: "io.example.keys", action: "peek", text: "Ctrl+Shift+F5"))
        let bound = { core.view.pluginShortcuts.first { $0.action == "io.example.keys.peek" } }
        try await waitFor("the user's own bound") { bound() != nil }
        let peek = try XCTUnwrap(bound())
        XCTAssertEqual(peek.text, "⇧⌘F5")
        XCTAssertEqual(peek.menuKey, UnicodeScalar(NSF5FunctionKey).map(Character.init))

        // Lua that never ends is stopped; an API call it may not make fails.
        let tab = try XCTUnwrap(core.window(window)?.active.id)
        let errors = { core.view.plugins.plugins.first { $0.id == "io.example.keys" }?.errors ?? [] }
        core.update(.runPluginAction(tab: tab, action: "io.example.keys.spin"))
        try await waitFor("the time limit") { errors().contains { $0.contains("took too long") } }
        core.update(.runPluginAction(tab: tab, action: "io.example.keys.peek"))
        try await waitFor("the permission check") {
            errors().contains { $0.contains("this needs the \"fs.read\" permission") }
        }

        try Data(manifest("Spin Faster").utf8).write(to: plugin.appendingPathComponent("plugin.toml"))
        try await waitFor("the edit reloaded") { spin()?.title == "Spin Faster" }
        XCTAssertTrue(core.view.toasts.contains { $0.text.hasPrefix("Plugin reloaded: ") })
    }

    func testSettingsApplyAndComeBackOnTheNextStart() async throws {
        let fm = FileManager.default
        let dir = fm.temporaryDirectory.appendingPathComponent("dirfleet_settings_\(UUID().uuidString)")
        try fm.createDirectory(at: dir, withIntermediateDirectories: true)
        defer { try? fm.removeItem(at: dir) }
        let stateFile = dir.appendingPathComponent("state.json").path

        let core = Core(picker: FakePicker(path: nil), stateFile: stateFile)
        core.update(.started(open: dir.path, home: NSHomeDirectory()))
        let trashShown = { core.view.snapshot.first?.sidebar.contains { $0.items.contains(where: \.trash) } ?? false }
        try await waitFor("the window and its Trash row") { trashShown() }
        var settings = core.view.settings
        settings.showTrash = false
        settings.iconSize = .large
        settings.appearance = .dark
        core.update(.changeSettings(settings: settings))
        try await waitFor("the settings applied") { core.view.settings.iconSize == .large && !trashShown() }
        try await waitFor("the save") { fm.fileExists(atPath: stateFile) }
        core.update(.quitting)

        let again = Core(picker: FakePicker(path: nil), stateFile: stateFile)
        again.update(.started(open: nil, home: NSHomeDirectory()))
        try await waitFor("the settings back") { again.view.settings.appearance == .dark }
        XCTAssertEqual(again.view.settings.iconSize, .large)
        XCTAssertFalse(again.view.settings.showTrash)
    }

    func testASavedSearchShowsInTheSidebarAndRunsAgain() async throws {
        let fm = FileManager.default
        let dir = fm.temporaryDirectory.appendingPathComponent("dirfleet_saved_\(UUID().uuidString)")
        try fm.createDirectory(at: dir, withIntermediateDirectories: true)
        defer { try? fm.removeItem(at: dir) }
        for name in ["report.pdf", "notes.txt"] {
            try Data("x".utf8).write(to: dir.appendingPathComponent(name))
        }

        let core = Core(picker: FakePicker(path: nil))
        core.update(.newWindow(path: dir.path))
        try await waitFor("the folder") { core.window(1)?.active.totalRows == 2 }
        core.update(.setSearchText(tab: 1, text: "report"))
        core.update(.saveSearch(tab: 1, name: "Reports"))
        try await waitFor("the saved search") { !core.view.savedSearches.isEmpty }
        let row = { core.window(1)?.sidebar.first { $0.section == .savedSearches }?.items.first }
        XCTAssertEqual(row()?.title, "Reports")
        XCTAssertEqual(row()?.active, true)

        core.update(.closeSearch(tab: 1))
        try await waitFor("the whole folder") { core.window(1)?.active.totalRows == 2 && core.window(1)?.active.search == nil }
        core.update(.runSavedSearch(tab: 1, id: core.view.savedSearches[0].id))
        try await waitFor("the saved search run") { core.window(1)?.active.totalRows == 1 }
        XCTAssertEqual(core.window(1)?.active.saved?.name, "Reports")
    }

    func testDroppedFilesMoveOnTheSameDriveOrCopyWithTheCopyKey() async throws {
        let fm = FileManager.default
        let dir = fm.temporaryDirectory.appendingPathComponent("dirfleet_drop_\(UUID().uuidString)")
        let from = dir.appendingPathComponent("from")
        let to = dir.appendingPathComponent("to")
        try fm.createDirectory(at: from, withIntermediateDirectories: true)
        try fm.createDirectory(at: to, withIntermediateDirectories: true)
        defer { try? fm.removeItem(at: dir) }
        for name in ["moved.txt", "copied.txt"] {
            try Data("x".utf8).write(to: from.appendingPathComponent(name))
        }

        let core = Core(picker: FakePicker(path: nil))
        core.update(.newWindow(path: to.path))
        try await waitFor("the folder") { core.window(1)?.active.loading == false }
        core.update(.dropItems(tab: 1, paths: [from.appendingPathComponent("moved.txt").path], target: nil, copy: false, forceMove: false))
        try await waitFor("the move") { fm.fileExists(atPath: to.appendingPathComponent("moved.txt").path) }
        XCTAssertFalse(fm.fileExists(atPath: from.appendingPathComponent("moved.txt").path))

        core.update(.dropItems(tab: 1, paths: [from.appendingPathComponent("copied.txt").path], target: nil, copy: true, forceMove: false))
        try await waitFor("the copy") { fm.fileExists(atPath: to.appendingPathComponent("copied.txt").path) }
        XCTAssertTrue(fm.fileExists(atPath: from.appendingPathComponent("copied.txt").path))
    }

    func testTypingSelectsByNameAndQuickLookStepsThroughTheFolder() async throws {
        let fm = FileManager.default
        let dir = fm.temporaryDirectory.appendingPathComponent("dirfleet_type_\(UUID().uuidString)")
        try fm.createDirectory(at: dir, withIntermediateDirectories: true)
        defer { try? fm.removeItem(at: dir) }
        for name in ["apple.txt", "banana.txt", "berry.txt"] {
            try Data("x".utf8).write(to: dir.appendingPathComponent(name))
        }
        let core = Core(picker: FakePicker(path: nil))
        core.update(.newWindow(path: dir.path))
        try await waitFor("the folder") { core.window(1)?.active.totalRows == 3 }
        core.update(.typeSelect(tab: 1, text: "be"))
        try await waitFor("berry selected") {
            core.window(1)?.active.rows.first(where: \.selected)?.name == "berry.txt"
        }
        core.update(.quickLook(tab: 1))
        try await waitFor("the preview") { !core.previewItems.isEmpty }
        XCTAssertEqual(core.previewItems.count, 3, "the folder's rows to step through")
        XCTAssertEqual(core.previewURL?.lastPathComponent, "berry.txt")
    }

    func testThePaletteFindsAFavoriteAndRunsIt() async throws {
        let fm = FileManager.default
        let dir = fm.temporaryDirectory.appendingPathComponent("dirfleet_palette_\(UUID().uuidString)")
        let work = dir.appendingPathComponent("Workbench")
        try fm.createDirectory(at: work, withIntermediateDirectories: true)
        defer { try? fm.removeItem(at: dir) }

        let core = Core(picker: FakePicker(path: nil))
        core.update(.newWindow(path: dir.path))
        try await waitFor("the folder") { core.window(1)?.active.loading == false }
        core.update(.addFavorite(path: work.path))
        core.update(.setPaletteText(text: "workb"))
        try await waitFor("the match") { core.view.palette?.items.first != nil }
        let first = try XCTUnwrap(core.view.palette?.items.first)
        XCTAssertEqual(first.target, .folder(work.path))
        core.run(first.target, tab: 1, window: 1)
        try await waitFor("the favorite opened") { core.window(1)?.active.title == "Workbench" }
        XCTAssertNil(core.view.palette)
        XCTAssertFalse(core.view.commands.contains { $0.shortcut.contains("Ctrl") })
    }

    func testCancelledPickChangesNothing() async throws {
        let core = Core(picker: FakePicker(path: nil))
        core.update(.newWindow(path: nil))
        core.update(.openFolder(tab: 1))
        try await Task.sleep(for: .milliseconds(200))
        XCTAssertEqual(core.window(1)?.active.path, "")
        XCTAssertEqual(core.window(1)?.active.loading, false)
    }

    func testWindowsComeBackOnTheNextStart() async throws {
        let dir = FileManager.default.temporaryDirectory
            .appendingPathComponent("dirfleet_state_\(UUID().uuidString)")
        let folder = dir.appendingPathComponent("home")
        try FileManager.default.createDirectory(
            at: folder.appendingPathComponent("inside"), withIntermediateDirectories: true
        )
        defer { try? FileManager.default.removeItem(at: dir) }
        let stateFile = dir.appendingPathComponent("state.json").path
        let inside = folder.appendingPathComponent("inside").path

        let first = Core(picker: FakePicker(path: nil), stateFile: stateFile)
        var window: UInt32?
        first.start(open: nil, home: folder.path) { window = $0 }
        try await waitFor("home") {
            first.window(window)?.active.path == folder.path && first.window(window)?.active.loading == false
        }
        let tab = try XCTUnwrap(first.window(window)?.active.id)
        first.update(.openRow(tab: tab, row: 0))
        try await waitFor("the saved subfolder") {
            (try? String(contentsOfFile: stateFile, encoding: .utf8))?.contains(inside) == true
        }

        let second = Core(picker: FakePicker(path: nil), stateFile: stateFile)
        var restored: UInt32?
        second.start(open: nil, home: "/") { restored = $0 }
        try await waitFor("the restored window") { second.window(restored)?.active.path == inside }
    }

    func testAChangeOnDiskShowsUp() async throws {
        let folder = FileManager.default.temporaryDirectory
            .appendingPathComponent("dirfleet_watch_\(UUID().uuidString)")
        try FileManager.default.createDirectory(at: folder, withIntermediateDirectories: true)
        defer { try? FileManager.default.removeItem(at: folder) }

        let core = Core(picker: FakePicker(path: nil))
        core.update(.newWindow(path: folder.path))
        try await waitFor("the empty folder") { core.window(1)?.active.loading == false }
        try Data("x".utf8).write(to: folder.appendingPathComponent("new.txt"))
        try await waitFor("the new file") { core.window(1)?.active.rows.map(\.name) == ["new.txt"] }
    }

    func testAMissingFolderShowsAnError() async throws {
        let core = Core(picker: FakePicker(path: nil))
        core.update(.newWindow(path: "/dirfleet-missing-\(UUID().uuidString)"))
        try await waitFor("the error") { core.window(1)?.active.error != nil }
        XCTAssertEqual(core.window(1)?.active.loading, false)
    }

    func testQuickLookShowsTheSelection() async throws {
        let folder = FileManager.default.temporaryDirectory
            .appendingPathComponent("dirfleet_look_\(UUID().uuidString)")
        try FileManager.default.createDirectory(at: folder, withIntermediateDirectories: true)
        defer { try? FileManager.default.removeItem(at: folder) }
        for name in ["a.txt", "b.txt", "c.txt"] {
            try Data(name.utf8).write(to: folder.appendingPathComponent(name))
        }

        let core = Core(picker: FakePicker(path: nil))
        core.update(.newWindow(path: folder.path))
        try await waitFor("the files") { core.window(1)?.active.totalRows == 3 }
        core.update(.click(tab: 1, row: 0, command: false, shift: false))
        core.update(.click(tab: 1, row: 2, command: true, shift: false))
        core.toggleQuickLook(tab: 1)
        try await waitFor("Quick Look") { core.previewURL != nil }
        XCTAssertEqual(core.previewItems.map(\.lastPathComponent), ["a.txt", "c.txt"])
        XCTAssertEqual(core.previewURL?.lastPathComponent, "c.txt", "starts at the cursor")
        XCTAssertEqual(core.previewTab, 1)
        core.toggleQuickLook(tab: 1)
        XCTAssertNil(core.previewURL, "the same key closes it")
    }

    /// Return in the panel opens the item it shows: here a folder, which the
    /// tab goes into (a file would open in its app).
    func testReturnInQuickLookOpensTheItemItShows() async throws {
        let fm = FileManager.default
        let folder = fm.temporaryDirectory.appendingPathComponent("dirfleet_look_open_\(UUID().uuidString)")
        try fm.createDirectory(at: folder.appendingPathComponent("inside"), withIntermediateDirectories: true)
        defer { try? fm.removeItem(at: folder) }
        try Data("x".utf8).write(to: folder.appendingPathComponent("a.txt"))

        let core = Core(picker: FakePicker(path: nil))
        core.update(.newWindow(path: folder.path))
        try await waitFor("the folder") { core.window(1)?.active.totalRows == 2 }
        core.update(.selectAll(tab: 1))
        core.toggleQuickLook(tab: 1)
        try await waitFor("Quick Look") { core.previewURL != nil }
        let inside = try XCTUnwrap(core.previewItems.first { $0.lastPathComponent == "inside" })
        // The panel's arrow keys move it, as here.
        core.previewURL = inside
        XCTAssertTrue(core.openPreviewed())
        XCTAssertNil(core.previewURL, "the panel closes")
        try await waitFor("inside the folder") { core.window(1)?.active.path.hasSuffix("/inside") == true }
        XCTAssertFalse(core.openPreviewed(), "nothing shown, nothing opened")
    }

    func testANewTabJoinsItsWindowsTabGroupAndTheGroupIsSaved() async throws {
        _ = NSApplication.shared
        let window = { () -> NSWindow in
            let window = NSWindow(
                contentRect: NSRect(x: 0, y: 0, width: 320, height: 240),
                styleMask: [.titled, .closable], backing: .buffered, defer: false
            )
            window.isReleasedWhenClosed = false
            window.tabbingMode = .preferred
            return window
        }
        let core = Core(picker: FakePicker(path: nil))
        var opened: [UInt32] = []
        core.openWindow = { opened.append($0) }
        core.update(.newWindow(path: nil))
        try await waitFor("the first window") { opened == [1] }
        let first = window()
        first.orderFront(nil)
        core.register(window: 1, nsWindow: first)

        core.newTab(of: 1, path: nil)
        try await waitFor("the new tab's window") { opened == [1, 2] }
        let second = window()
        core.register(window: 2, nsWindow: second)
        XCTAssertEqual(first.tabbedWindows?.count, 2, "joined the first window's group")

        core.reportGroups()
        try await waitFor("the groups") { core.view.snapshot.allSatisfy { $0.group == 1 } }
        first.close()
        second.close()
    }

    func testCoreOpenedWindowsAreShownAndLaunchWindowsAdopted() async throws {
        let core = Core(picker: FakePicker(path: nil))
        var opened: [UInt32] = []
        core.openWindow = { opened.append($0) }
        var adopted: UInt32?
        core.adopt(path: nil) { adopted = $0 }
        core.update(.newWindow(path: nil))
        try await waitFor("both windows") { core.view.snapshot.count == 2 && !opened.isEmpty }
        XCTAssertEqual(adopted, 1, "the launch window takes the first id")
        XCTAssertEqual(opened, [2], "only the core's own window is opened")

        core.update(.windowClosed(window: 1))
        try await waitFor("the closed window to go") { core.view.snapshot.map(\.id) == [2] }
    }
}
