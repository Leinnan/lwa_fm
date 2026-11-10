import App
import AppKit

/// What the file views draw with besides the tab: the list's columns and
/// the grid's icon size.
struct FileViewConfig: Equatable {
    var columns: [ColumnView]
    var optionalColumns: [ColumnChoice]
    var iconSize: IconSize
}

/// A tab's rows as AppKit draws them: the list or the grid.
@MainActor
protocol FileSurface: AnyObject {
    var scrollView: NSScrollView { get }
    /// The view that takes keys and holds the rename field.
    var keyView: NSView { get }
    /// Draws `tab`: only what differs from what is drawn.
    func show(_ tab: TabViewModel, config: FileViewConfig)
    /// Where `row`'s name is drawn, in `keyView`'s coordinates; nil when
    /// the row isn't drawn.
    func nameFrame(of row: UInt32) -> NSRect?
    /// Scrolls `row` to the top.
    func scroll(toTop row: UInt32)
}

extension TabViewModel {
    /// The row at position `index`, once the core has sent it.
    func row(at index: UInt32) -> RowView? {
        guard index >= firstRow else { return nil }
        let offset = Int(index - firstRow)
        return offset < rows.count ? rows[offset] : nil
    }

    /// Whether the row at `index` is selected, sent or not.
    func isSelected(_ index: UInt32) -> Bool {
        if let row = row(at: index) { return row.selected }
        // Ranges are in order: the first one ending after the row decides.
        var low = 0
        var high = selection.count
        while low < high {
            let middle = (low + high) / 2
            if selection[middle].end <= index { low = middle + 1 } else { high = middle }
        }
        return low < selection.count && selection[low].start <= index
    }

    /// The path of `row`, which a search result from another folder names
    /// below the tab's.
    func path(of row: RowView) -> String {
        let parent = row.folder.hasPrefix("/") ? row.folder : (path as NSString).appendingPathComponent(row.folder)
        return (parent as NSString).appendingPathComponent(row.name)
    }
}

extension RowView {
    /// Folders and apps take dropped files; a drop on a file goes to the
    /// folder around it.
    var takesDrops: Bool { isDir || name.hasSuffix(".app") }
}

/// One pane's rows, drawn by AppKit: a table for the list and a grid of
/// tiles for the icons, fed by the tab's store without passing through
/// SwiftUI, so a click, a thumbnail or a scroll step costs what the rows in
/// sight cost and nothing else. Selection, sorting and renaming stay the
/// core's; this sends it what the user does and draws what it answers.
@MainActor
final class FolderController: NSObject, TabRowsObserver, NSTextFieldDelegate {
    let core: Core
    /// What SwiftUI hosts; the list or the grid fills it.
    let view = NSView()
    private(set) var tab: TabViewModel?
    private var config = FileViewConfig(columns: [], optionalColumns: [], iconSize: .medium)
    private weak var store: TabStore?
    private var list: FileList?
    private var grid: FileGrid?
    private(set) var surface: FileSurface?
    /// The rows in sight, told to the core once per turn of the run loop.
    private var viewport: (first: UInt32, count: UInt32)?
    private var viewportDue = false
    private var reported: (tab: UInt32, first: UInt32, count: UInt32)?
    /// The rename field, and the row and name it was opened with.
    private var renameField: NSTextField?
    private var renaming: RenameView?
    private let dropOverlay = DropOverlay()

    init(core: Core) {
        self.core = core
        super.init()
        dropOverlay.isHidden = true
        LaunchTime.mark("rows_view")
    }

    // MARK: The tab

    /// Draws `store`'s tab from now on.
    func attach(_ store: TabStore) {
        guard self.store !== store else { return }
        detach()
        self.store = store
        store.rows = self
        apply(store.content)
    }

    func detach() {
        if store?.rows === self { store?.rows = nil }
        store = nil
    }

    func configure(_ config: FileViewConfig) {
        guard config != self.config else { return }
        self.config = config
        if let tab { surface?.show(tab, config: config) }
    }

    func apply(_ new: TabViewModel) {
        let old = tab
        tab = new
        showSurface(new.view.display)
        surface?.show(new, config: config)
        if !new.rows.isEmpty { LaunchTime.rowsShown(in: view) }
        if let old, old.id == new.id {
            if old.path != new.path || old.inTrash != new.inTrash {
                surface?.scroll(toTop: 0)
            } else if old.scrollCount != new.scrollCount, let row = new.scrollTo, row < new.totalRows {
                surface?.scroll(toTop: row)
            }
        }
        syncRename()
    }

    /// The list or the grid, made when first shown.
    private func showSurface(_ display: DisplayType) {
        let wanted: FileSurface
        switch display {
        case .list:
            if list == nil { list = FileList(owner: self) }
            wanted = list!
        case .icons:
            if grid == nil { grid = FileGrid(owner: self) }
            wanted = grid!
        }
        guard wanted !== surface else { return }
        let focused = surface.map { view.window?.firstResponder === $0.keyView } ?? false
        endRename()
        surface?.scrollView.removeFromSuperview()
        surface = wanted
        let scroll = wanted.scrollView
        scroll.frame = view.bounds
        scroll.autoresizingMask = [.width, .height]
        view.addSubview(scroll)
        dropOverlay.frame = view.bounds
        dropOverlay.autoresizingMask = [.width, .height]
        view.addSubview(dropOverlay)
        reported = nil
        if focused { view.window?.makeFirstResponder(wanted.keyView) }
    }

    // MARK: What the user does

    /// The rows in sight changed.
    func rowsInSight(first: UInt32, count: UInt32) {
        viewport = (first, count)
        guard !viewportDue else { return }
        viewportDue = true
        DispatchQueue.main.async { [weak self] in
            guard let self else { return }
            viewportDue = false
            guard let tab, let (first, count) = viewport else { return }
            if let reported, reported == (tab.id, first, count) { return }
            reported = (tab.id, first, count)
            core.update(.viewportChanged(tab: tab.id, firstRow: first, rows: count))
        }
    }

    /// A click in this pane makes its tab the window's focused one.
    func activate() {
        guard let tab, let window = core.window(showing: tab.id), window.active.id != tab.id else { return }
        core.update(.activateTab(tab: tab.id))
    }

    /// Opens a row; ⌘ opens a folder in a new tab, as in Finder.
    func open(row: UInt32, command: Bool) {
        guard let tab else { return }
        if command, let shown = tab.row(at: row), shown.isDir, let window = core.window(showing: tab.id) {
            core.newTab(of: window.id, path: tab.path(of: shown))
        } else {
            core.update(.openRow(tab: tab.id, row: row))
        }
    }

    /// The keys the list and the grid share; false for a key that isn't
    /// theirs.
    func keyDown(_ event: NSEvent) -> Bool {
        guard let tab else { return false }
        let flags = event.modifierFlags.intersection(.deviceIndependentFlagsMask)
        guard flags.isDisjoint(with: [.command, .control]) else { return false }
        switch event.keyCode {
        case 49: // Space
            core.toggleQuickLook(tab: tab.id)
        case 36, 76: // Return renames, as in Finder.
            guard tab.renaming == nil, !tab.inTrash, tab.selectedCount > 0 else { return false }
            core.update(.startRename(tab: tab.id))
        case 53: // Escape with the search open: clear, then close it.
            guard tab.search != nil else { return false }
            core.update(.searchEscape(tab: tab.id))
        case 51, 117: // Delete
            if !tab.inTrash { core.trashSelection(tab: tab.id) }
        default:
            // Letters typed jump to matching names.
            guard !flags.contains(.option), let typed = event.characters, !typed.isEmpty,
                  typed.unicodeScalars.allSatisfy({ Self.typed.contains($0) }) else { return false }
            core.update(.typeSelect(tab: tab.id, text: typed))
            core.refreshNow()
        }
        return true
    }

    private static let typed = CharacterSet.alphanumerics.union(.punctuationCharacters)

    /// The menu of `row`, or of the folder's empty space. A right-click on
    /// a row outside the selection selects it first, so the menu acts on it.
    func menu(forRow row: UInt32?) -> NSMenu? {
        guard let id = tab?.id else { return nil }
        if let row {
            core.update(.contextClick(tab: id, row: row))
            core.refreshNow()
        }
        guard let tab else { return nil }
        if row == nil {
            return NSMenu(entries: FileMenus.emptyArea(core: core, tab: tab))
        }
        return NSMenu(entries: tab.inTrash ? FileMenus.trashRow(core: core, tab: tab) : FileMenus.row(core: core, tab: tab))
    }

    /// The Edit menu's commands, for the view with the keys.
    func copySelection() { if let tab { core.update(.copySelection(tab: tab.id)) } }
    func cutSelection() { if let tab { core.update(.cutSelection(tab: tab.id)) } }
    func pasteItems() { if let tab { core.update(.paste(tab: tab.id)) } }
    func selectAll() {
        guard let tab else { return }
        core.update(.selectAll(tab: tab.id))
        core.refreshNow()
    }

    func deleteSelection() {
        if let tab, !tab.inTrash { core.trashSelection(tab: tab.id) }
    }

    /// Whether an Edit menu command applies now.
    func validate(_ action: Selector?) -> Bool {
        guard let tab else { return false }
        switch action {
        case #selector(NSText.copy(_:)): return tab.selectedCount > 0
        case #selector(NSText.cut(_:)), #selector(NSText.delete(_:)): return tab.selectedCount > 0 && !tab.inTrash
        case #selector(NSText.paste(_:)): return !tab.inTrash && !tab.path.isEmpty
        default: return true
        }
    }

    // MARK: Drag and drop

    /// What a drag of `row` carries: its file, when the core has sent it.
    func dragItem(row: UInt32) -> NSPasteboardWriting? {
        guard let tab, !tab.inTrash, let shown = tab.row(at: row) else { return nil }
        return NSURL(fileURLWithPath: tab.path(of: shown), isDirectory: shown.isDir)
    }

    /// Where a drop at `row` would go: into that row when it is a folder
    /// or an app, else into the tab's folder (nil), and how. `.none` when
    /// nothing can be dropped here.
    func dropTarget(_ info: NSDraggingInfo, row: UInt32?) -> (row: UInt32?, operation: NSDragOperation) {
        guard let tab, !tab.inTrash, info.draggingPasteboard.canReadObject(
            forClasses: [NSURL.self], options: [.urlReadingFileURLsOnly: true]
        ) else { return (nil, []) }
        let allowed = info.draggingSourceOperationMask
        let operation: NSDragOperation = NSEvent.modifierFlags.contains(.option) && allowed.contains(.copy) ? .copy
            : allowed.contains(.move) ? .move
            : allowed.contains(.generic) ? .generic : allowed.contains(.copy) ? .copy : []
        if let row, let shown = tab.row(at: row), shown.takesDrops {
            // Not onto one of the dragged items itself.
            let target = tab.path(of: shown)
            if !dropped(info).contains(where: { $0.path == target }) {
                return (row, operation)
            }
        }
        return tab.path.isEmpty ? (nil, []) : (nil, operation)
    }

    /// Shows that a drop would go into the tab's folder, as egui's outline.
    func showFolderDrop(_ shown: Bool) {
        guard dropOverlay.isHidden == shown else { return }
        dropOverlay.title = tab.map { $0.path.isEmpty ? nil : $0.title } ?? nil
        dropOverlay.isHidden = !shown
    }

    /// Takes the drop: into `row`, or into the tab's folder.
    func drop(_ info: NSDraggingInfo, row: UInt32?) -> Bool {
        guard let tab else { return false }
        let urls = dropped(info)
        guard !urls.isEmpty else { return false }
        let target = row.flatMap { tab.row(at: $0) }.map { tab.path(of: $0) }
        let id = tab.id
        // After the drag ends: the core's question about moving or copying
        // is a dialog.
        DispatchQueue.main.async { [core] in
            core.dropItems(tab: id, urls: urls, target: target)
        }
        return true
    }

    private func dropped(_ info: NSDraggingInfo) -> [URL] {
        info.draggingPasteboard.readObjects(forClasses: [NSURL.self], options: [.urlReadingFileURLsOnly: true]) as? [URL] ?? []
    }

    // MARK: Rename

    /// Shows the rename field on the row being renamed, while it is drawn.
    private func syncRename() {
        guard let tab, let surface, let wanted = tab.renaming, let frame = surface.nameFrame(of: wanted.row) else {
            endRename()
            return
        }
        let field: NSTextField
        if let renameField {
            field = renameField
        } else {
            field = NSTextField()
            field.isBordered = true
            field.isBezeled = true
            field.bezelStyle = .roundedBezel
            field.font = .systemFont(ofSize: NSFont.systemFontSize)
            field.delegate = self
            field.cell?.isScrollable = true
            field.cell?.usesSingleLineMode = true
            renameField = field
        }
        if field.superview !== surface.keyView {
            surface.keyView.addSubview(field)
        }
        field.frame = frame
        // A new rename, not the same one drawn again: what was typed stays.
        if renaming?.row != wanted.row || renaming?.name != wanted.name {
            field.stringValue = wanted.name
            field.window?.makeFirstResponder(field)
            // The name without its extension, as Finder selects it.
            let stem = (wanted.name as NSString).deletingPathExtension
            field.currentEditor()?.selectedRange = NSRange(location: 0, length: (stem as NSString).length)
        }
        field.toolTip = wanted.error
        field.textColor = wanted.error == nil ? .labelColor : .systemRed
        renaming = wanted
    }

    private func endRename() {
        guard let field = renameField, field.superview != nil else {
            renaming = nil
            return
        }
        let focused = field.currentEditor() != nil
        let keys = field.superview
        field.removeFromSuperview()
        renaming = nil
        if focused, let keys { keys.window?.makeFirstResponder(keys) }
    }

    func control(_: NSControl, textView _: NSTextView, doCommandBy selector: Selector) -> Bool {
        guard let tab else { return false }
        switch selector {
        case #selector(NSResponder.insertNewline(_:)):
            core.update(.rename(tab: tab.id, name: renameField?.stringValue ?? ""))
            return true
        case #selector(NSResponder.cancelOperation(_:)):
            core.update(.cancelRename(tab: tab.id))
            return true
        default:
            return false
        }
    }
}

/// How long the app took from its start to its first rows on screen, and
/// to the steps on the way, printed once when `DIRFLEET_LAUNCH_TIME` is
/// set: for measuring launches of the built app
/// (`docs/crux-migration/perf.md`).
@MainActor
enum LaunchTime {
    private static var pending = ProcessInfo.processInfo.environment["DIRFLEET_LAUNCH_TIME"] != nil
    private static var marks: [(String, Double)] = []

    /// Notes that the launch got to `step`, the first time it does.
    static func mark(_ step: String) {
        guard pending, !marks.contains(where: { $0.0 == step }) else { return }
        marks.append((step, sinceStart()))
    }

    static func rowsShown(in view: NSView) {
        guard pending else { return }
        mark("first_rows")
        pending = false
        // Once more after the frame with the rows has been drawn.
        DispatchQueue.main.async {
            view.window?.displayIfNeeded()
            CATransaction.flush()
            marks.append(("drawn", sinceStart()))
            let steps = marks.map { String(format: "%@_ms=%.0f", $0.0, $0.1 * 1000) }
            print("LAUNCH " + steps.joined(separator: " "))
            fflush(stdout)
        }
    }

    /// Seconds since the process started, as the kernel has it.
    private static func sinceStart() -> Double {
        var info = kinfo_proc()
        var size = MemoryLayout<kinfo_proc>.stride
        var name = [CTL_KERN, KERN_PROC, KERN_PROC_PID, getpid()]
        guard sysctl(&name, 4, &info, &size, nil, 0) == 0 else { return 0 }
        let start = info.kp_proc.p_starttime
        return Date().timeIntervalSince1970 - (Double(start.tv_sec) + Double(start.tv_usec) / 1e6)
    }
}

/// The outline over a folder that a drop would go into, with what it says.
private final class DropOverlay: NSView {
    var title: String? {
        didSet { needsDisplay = true }
    }

    override func hitTest(_: NSPoint) -> NSView? { nil }

    override func draw(_: NSRect) {
        let shape = NSBezierPath(roundedRect: bounds.insetBy(dx: 5, dy: 5), xRadius: 8, yRadius: 8)
        NSColor.controlAccentColor.withAlphaComponent(0.12).setFill()
        shape.fill()
        NSColor.controlAccentColor.setStroke()
        shape.lineWidth = 2
        shape.stroke()
        let text = title.map { "Drop into \($0)" } ?? "Open a single folder to drop items into it"
        let attributes: [NSAttributedString.Key: Any] = [
            .font: NSFont.systemFont(ofSize: 15, weight: .semibold),
            .foregroundColor: NSColor.labelColor,
        ]
        let size = (text as NSString).size(withAttributes: attributes)
        (text as NSString).draw(
            at: NSPoint(x: bounds.midX - size.width / 2, y: bounds.midY - size.height / 2),
            withAttributes: attributes
        )
    }
}
