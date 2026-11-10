import App
import AppKit

/// The list of a tab's rows: an `NSTableView` with rows of one height and
/// reused cells. It has as many rows as the tab; a row the core hasn't sent
/// yet is drawn empty. The table selects and sorts by itself, as tables do,
/// and tells the core, whose answer it then shows.
@MainActor
final class FileList: NSObject, FileSurface, NSTableViewDataSource, NSTableViewDelegate, NSMenuDelegate {
    let scrollView = NSScrollView()
    private let table = FileTableView()
    fileprivate unowned let owner: FolderController
    private var tab: TabViewModel?
    private var config: FileViewConfig?
    /// Each column's kind, and where its text is in a row's cells.
    private var kinds: [String: Column] = [:]
    private var cellIndex: [String: Int] = [:]
    private var sorts: [String: Sort] = [:]
    /// The table is being changed to show the core's view, not by the
    /// user: nothing is sent back.
    private var programmatic = false
    private var observers: [NSObjectProtocol] = []
    private let rowMenu = NSMenu()
    private let headerMenu = NSMenu()

    var keyView: NSView { table }

    init(owner: FolderController) {
        self.owner = owner
        super.init()
        table.list = self
        table.dataSource = self
        table.delegate = self
        table.style = .inset
        table.usesAlternatingRowBackgroundColors = true
        table.rowHeight = 24
        table.usesAutomaticRowHeights = false
        table.allowsMultipleSelection = true
        table.allowsEmptySelection = true
        table.allowsColumnReordering = false
        table.allowsColumnSelection = false
        table.allowsTypeSelect = false
        table.columnAutoresizingStyle = .firstColumnOnlyAutoresizingStyle
        table.target = self
        table.doubleAction = #selector(doubleClicked)
        table.setDraggingSourceOperationMask([.copy, .move, .generic, .link], forLocal: false)
        table.setDraggingSourceOperationMask([.copy, .move, .generic], forLocal: true)
        table.registerForDraggedTypes([.fileURL])
        rowMenu.delegate = self
        table.menu = rowMenu
        headerMenu.delegate = self
        table.headerView?.menu = headerMenu

        scrollView.documentView = table
        scrollView.hasVerticalScroller = true
        scrollView.hasHorizontalScroller = true
        scrollView.autohidesScrollers = true
        let clip = scrollView.contentView
        clip.postsBoundsChangedNotifications = true
        clip.postsFrameChangedNotifications = true
        for name in [NSView.boundsDidChangeNotification, NSView.frameDidChangeNotification] {
            observers.append(NotificationCenter.default.addObserver(forName: name, object: clip, queue: .main) { [weak self] _ in
                MainActor.assumeIsolated { self?.reportViewport() }
            })
        }
    }

    deinit {
        for observer in observers { NotificationCenter.default.removeObserver(observer) }
    }

    // MARK: Showing the tab

    func show(_ new: TabViewModel, config new_config: FileViewConfig) {
        let old = tab
        let oldConfig = config
        tab = new
        config = new_config
        programmatic = true
        defer { programmatic = false }

        let columnsChanged = syncColumns(new, new_config, was: old, oldConfig)
        if columnsChanged || old?.view.sort != new.view.sort || old?.view.invert != new.view.invert {
            syncSort(new)
        }
        if columnsChanged || old == nil || old?.id != new.id || old?.path != new.path {
            table.reloadData()
        } else if let old {
            if Int(new.totalRows) != table.numberOfRows {
                table.noteNumberOfRowsChanged()
            }
            // Only the rows drawn, and of those only the ones that differ.
            let renamed = old.renaming?.row != new.renaming?.row
            table.enumerateAvailableRowViews { view, row in
                let index = UInt32(row)
                let shown = new.row(at: index)
                if !renamed, shown == old.row(at: index) { return }
                for column in 0..<view.numberOfColumns {
                    self.fill(view.view(atColumn: column) as? NSView, row: shown, in: new)
                }
            }
        }
        if old?.selection != new.selection || Int(new.totalRows) != Int(old?.totalRows ?? 0) || old?.id != new.id {
            syncSelection(new)
        }
        reportViewport()
    }

    /// Makes the table's columns the core's: Name, Folder while results
    /// from several folders show, then the shown columns in the core's
    /// order, at its widths. True when columns came or went.
    private func syncColumns(_ tab: TabViewModel, _ config: FileViewConfig, was old: TabViewModel?, _ oldConfig: FileViewConfig?) -> Bool {
        var wanted: [(id: String, column: Column, title: String, width: CGFloat, sort: Sort?)] = [
            ("name", .name, "Name", 0, .name),
        ]
        if tab.showFolder {
            wanted.append(("folder", .folder, "Folder", 200, nil))
        }
        var index: [String: Int] = [:]
        for (offset, column) in config.columns.dropFirst().enumerated() where column.column != .name && column.column != .folder {
            let id = Self.id(of: column.column)
            index[id] = offset
            // In the Trash the date column holds when the item was deleted.
            let title = column.column == .modified && tab.inTrash ? "Deleted" : column.title
            wanted.append((id, column.column, title, CGFloat(column.width), column.sort))
        }
        cellIndex = index
        let ids = wanted.map(\.id)
        let changed = table.tableColumns.map(\.identifier.rawValue) != ids
        if changed {
            for column in table.tableColumns { table.removeTableColumn(column) }
            kinds = [:]
            sorts = [:]
            for item in wanted {
                let column = NSTableColumn(identifier: NSUserInterfaceItemIdentifier(item.id))
                if item.column == .name {
                    column.minWidth = 140
                    column.width = 320
                    column.resizingMask = [.autoresizingMask, .userResizingMask]
                } else {
                    column.minWidth = 56
                    column.maxWidth = 600
                    column.width = max(56, item.width)
                    column.resizingMask = .userResizingMask
                }
                if let sort = item.sort {
                    column.sortDescriptorPrototype = NSSortDescriptor(key: item.id, ascending: true)
                    sorts[item.id] = sort
                }
                if item.column == .size || item.column == .openCount {
                    column.headerCell.alignment = .right
                }
                kinds[item.id] = item.column
                table.addTableColumn(column)
            }
            table.sizeToFit()
        }
        // Titles and widths, unless the user is dragging a column's edge now.
        let resizing = (table.headerView?.resizedColumn ?? -1) >= 0
        for (item, column) in zip(wanted, table.tableColumns) {
            if column.title != item.title { column.title = item.title }
            if !changed, !resizing, item.column != .name, item.column != .folder, item.width >= 56,
               abs(column.width - item.width) >= 1 {
                column.width = item.width
            }
        }
        return changed
    }

    private static func id(of column: Column) -> String {
        switch column {
        case .name: "name"
        case .folder: "folder"
        case .modified: "modified"
        case .created: "created"
        case .accessed: "opened"
        case .openCount: "opens"
        case .kind: "kind"
        case .size: "size"
        }
    }

    /// Shows the core's sort in the header.
    private func syncSort(_ tab: TabViewModel) {
        let sort = tab.view.sort
        var key = sorts.first { $0.value == sort }?.key
        // Random has no column, and most opened may have none shown.
        if key == nil, sort == .mostOpened, sorts["opened"] != nil { key = "opened" }
        if key == nil, sort == .name || sort == .random { key = "name" }
        let wanted = key.map { [NSSortDescriptor(key: $0, ascending: !tab.view.invert)] } ?? []
        if table.sortDescriptors != wanted { table.sortDescriptors = wanted }
    }

    /// Selects the core's selection, which comes as ranges.
    private func syncSelection(_ tab: TabViewModel) {
        var rows = IndexSet()
        for range in tab.selection where range.start < range.end {
            rows.insert(integersIn: Int(range.start)..<Int(range.end))
        }
        if rows != table.selectedRowIndexes {
            table.selectRowIndexes(rows, byExtendingSelection: false)
        }
    }

    private func reportViewport() {
        guard tab != nil, table.numberOfRows > 0 else { return }
        let rows = table.rows(in: table.visibleRect)
        guard rows.length > 0 else { return }
        owner.rowsInSight(first: UInt32(rows.location), count: UInt32(rows.length))
    }

    func nameFrame(of row: UInt32) -> NSRect? {
        let index = Int(row)
        guard index < table.numberOfRows, table.numberOfColumns > 0,
              table.rows(in: table.visibleRect).contains(index) else { return nil }
        var frame = table.frameOfCell(atColumn: 0, row: index)
        frame.origin.x += NameCell.textLeft - 4
        frame.size.width -= NameCell.textLeft - 4
        return frame.insetBy(dx: 0, dy: 1)
    }

    func scroll(toTop row: UInt32) {
        guard Int(row) < table.numberOfRows else { return }
        let clip = scrollView.contentView
        let top = scrollView.contentInsets.top
        let bottom = max(-top, table.frame.height - clip.bounds.height + scrollView.contentInsets.bottom)
        let y = min(table.rect(ofRow: Int(row)).minY - top, bottom)
        clip.scroll(to: NSPoint(x: clip.bounds.minX, y: y))
        scrollView.reflectScrolledClipView(clip)
    }

    // MARK: Rows and cells

    func numberOfRows(in _: NSTableView) -> Int {
        Int(tab?.totalRows ?? 0)
    }

    func tableView(_ tableView: NSTableView, viewFor column: NSTableColumn?, row: Int) -> NSView? {
        guard let tab, let column else { return nil }
        let id = column.identifier
        let cell: NSView
        if let reused = tableView.makeView(withIdentifier: id, owner: nil) {
            cell = reused
        } else if id.rawValue == "name" {
            cell = NameCell(identifier: id)
        } else {
            cell = TextCell(identifier: id, column: kinds[id.rawValue] ?? .kind)
        }
        fill(cell, row: tab.row(at: UInt32(row)), in: tab)
        return cell
    }

    private func fill(_ cell: NSView?, row: RowView?, in tab: TabViewModel) {
        if let name = cell as? NameCell {
            let renaming = row.map { tab.renaming?.row == $0.index } ?? false
            name.show(row, path: row.map { tab.path(of: $0) }, renaming: renaming,
                      scale: table.window?.backingScaleFactor ?? 2)
        } else if let text = cell as? TextCell, let id = text.identifier?.rawValue {
            if id == "folder" {
                text.show(row?.folder ?? "")
            } else if let row, let index = cellIndex[id], index < row.cells.count {
                text.show(row.cells[index])
            } else {
                text.show("")
            }
        }
    }

    // MARK: What the user does

    func tableViewSelectionDidChange(_: Notification) {
        guard !programmatic, let tab else { return }
        owner.core.update(.setSelection(tab: tab.id, rows: table.selectedRowIndexes.map { UInt32($0) }))
        // Now, so the core's selection and the table's never differ when a
        // later view comes.
        owner.core.refreshNow()
    }

    /// A header was clicked: the core sorts.
    func tableView(_ tableView: NSTableView, sortDescriptorsDidChange _: [NSSortDescriptor]) {
        guard !programmatic, let tab, let first = tableView.sortDescriptors.first,
              let key = first.key, let sort = sorts[key] else { return }
        let reverse = !first.ascending
        let core = owner.core
        if sort != tab.view.sort {
            core.update(.sortBy(tab: tab.id, sort: sort))
            if reverse { core.update(.setReverse(tab: tab.id, reverse: true)) }
        } else if reverse != tab.view.invert {
            core.update(.setReverse(tab: tab.id, reverse: reverse))
        }
    }

    /// The user dragged a column's edge: the core keeps the width. Columns
    /// the table resizes to fit the window aren't told.
    func tableViewColumnDidResize(_ notification: Notification) {
        guard !programmatic, (table.headerView?.resizedColumn ?? -1) >= 0,
              let column = notification.userInfo?["NSTableColumn"] as? NSTableColumn,
              let kind = kinds[column.identifier.rawValue], kind != .name else { return }
        owner.core.update(.setColumnWidth(column: kind, width: UInt32(column.width.rounded())))
    }

    @objc private func doubleClicked() {
        let row = table.clickedRow
        guard row >= 0 else { return }
        owner.open(row: UInt32(row), command: NSEvent.modifierFlags.contains(.command))
    }

    /// The rows' menu and the header's are built when they open.
    func menuNeedsUpdate(_ menu: NSMenu) {
        menu.removeAllItems()
        if menu === headerMenu {
            for choice in config?.optionalColumns ?? [] {
                let item = NSMenuItem(title: choice.title, action: #selector(toggleColumn(_:)), keyEquivalent: "")
                item.target = self
                item.state = choice.shown ? .on : .off
                item.representedObject = Self.id(of: choice.column)
                menu.addItem(item)
            }
            return
        }
        let row = table.clickedRow
        guard let built = owner.menu(forRow: row >= 0 ? UInt32(row) : nil) else { return }
        for item in built.items {
            built.removeItem(item)
            menu.addItem(item)
        }
    }

    @objc private func toggleColumn(_ sender: NSMenuItem) {
        guard let id = sender.representedObject as? String,
              let choice = config?.optionalColumns.first(where: { Self.id(of: $0.column) == id }) else { return }
        owner.core.update(.toggleColumn(column: choice.column))
    }

    // MARK: Drag and drop

    func tableView(_: NSTableView, pasteboardWriterForRow row: Int) -> NSPasteboardWriting? {
        owner.dragItem(row: UInt32(row))
    }

    func tableView(
        _ tableView: NSTableView, validateDrop info: NSDraggingInfo, proposedRow row: Int,
        proposedDropOperation operation: NSTableView.DropOperation
    ) -> NSDragOperation {
        let over: UInt32? = operation == .on && row >= 0 && row < tableView.numberOfRows ? UInt32(row) : nil
        let target = owner.dropTarget(info, row: over)
        // Onto a folder's row, or into the folder shown: never between rows.
        tableView.setDropRow(target.row.map { Int($0) } ?? -1, dropOperation: .on)
        owner.showFolderDrop(target.row == nil && !target.operation.isEmpty)
        return target.operation
    }

    func tableView(
        _ tableView: NSTableView, acceptDrop info: NSDraggingInfo, row: Int,
        dropOperation operation: NSTableView.DropOperation
    ) -> Bool {
        owner.showFolderDrop(false)
        let over: UInt32? = operation == .on && row >= 0 && row < tableView.numberOfRows ? UInt32(row) : nil
        return owner.drop(info, row: over)
    }
}

/// The list's table: keys, the Edit menu's commands and clicks that the
/// folder's controller answers.
final class FileTableView: NSTableView {
    fileprivate weak var list: FileList?

    override func keyDown(with event: NSEvent) {
        if list?.owner.keyDown(event) == true { return }
        super.keyDown(with: event)
    }

    override func mouseDown(with event: NSEvent) {
        list?.owner.activate()
        super.mouseDown(with: event)
    }

    override func viewDidMoveToWindow() {
        super.viewDidMoveToWindow()
        // The folder takes the keys of a window nothing else has taken.
        if let window, window.firstResponder === window {
            window.makeFirstResponder(self)
        }
    }

    /// Every row at once is the core's to select: it knows them all.
    override func selectAll(_: Any?) {
        list?.owner.selectAll()
    }

    @objc func copy(_: Any?) { list?.owner.copySelection() }
    @objc func cut(_: Any?) { list?.owner.cutSelection() }
    @objc func paste(_: Any?) { list?.owner.pasteItems() }
    @objc func delete(_: Any?) { list?.owner.deleteSelection() }

    override func validateUserInterfaceItem(_ item: NSValidatedUserInterfaceItem) -> Bool {
        switch item.action {
        case #selector(copy(_:)), #selector(cut(_:)), #selector(paste(_:)), #selector(delete(_:)):
            return list?.owner.validate(item.action) ?? false
        default:
            return super.validateUserInterfaceItem(item)
        }
    }

    override func draggingExited(_ sender: NSDraggingInfo?) {
        list?.owner.showFolderDrop(false)
        super.draggingExited(sender)
    }

    override func draggingEnded(_ sender: NSDraggingInfo) {
        list?.owner.showFolderDrop(false)
        super.draggingEnded(sender)
    }
}

/// A cell of one line of text, placed by frame: no Auto Layout to solve
/// for every row scrolled in.
private class TextCell: NSTableCellView {
    let label = NSTextField(labelWithString: "")
    private var shown = ""
    /// The height of a line of the list's font.
    static let lineHeight: CGFloat = {
        let font = NSFont.systemFont(ofSize: NSFont.systemFontSize)
        return ceil(font.ascender - font.descender + font.leading) + 1
    }()

    init(identifier: NSUserInterfaceItemIdentifier, column: Column) {
        super.init(frame: .zero)
        self.identifier = identifier
        label.lineBreakMode = .byTruncatingTail
        label.cell?.usesSingleLineMode = true
        switch column {
        case .modified, .created, .accessed:
            label.font = .monospacedDigitSystemFont(ofSize: NSFont.systemFontSize, weight: .regular)
        case .size, .openCount:
            label.font = .monospacedDigitSystemFont(ofSize: NSFont.systemFontSize, weight: .regular)
            label.alignment = .right
        case .folder:
            label.textColor = .secondaryLabelColor
        case .name, .kind:
            break
        }
        textField = label
        addSubview(label)
    }

    @available(*, unavailable)
    required init?(coder _: NSCoder) { fatalError("made in code") }

    override func setFrameSize(_ size: NSSize) {
        super.setFrameSize(size)
        place()
    }

    func place() {
        label.frame = NSRect(x: textLeft, y: ((bounds.height - Self.lineHeight) / 2).rounded(),
                             width: max(0, bounds.width - textLeft - 2), height: Self.lineHeight)
    }

    var textLeft: CGFloat { 2 }

    func show(_ text: String) {
        guard text != shown else { return }
        shown = text
        label.stringValue = text
    }
}

/// The Name cell: the file's icon or thumbnail, then its name with the
/// search's matches marked.
private final class NameCell: TextCell {
    static let textLeft: CGFloat = 26
    private let icon = NSView()
    private var row: RowView?
    private var path: String?
    private var thumbnail = ""
    private var ticket: ThumbnailLoader.Ticket?
    private var nameHidden = false

    init(identifier: NSUserInterfaceItemIdentifier) {
        super.init(identifier: identifier, column: .name)
        icon.wantsLayer = true
        icon.layer?.contentsGravity = .resizeAspect
        addSubview(icon)
    }

    override var textLeft: CGFloat { Self.textLeft }

    override func place() {
        super.place()
        icon.frame = NSRect(x: 4, y: ((bounds.height - 16) / 2).rounded(), width: 16, height: 16)
    }

    func show(_ new: RowView?, path newPath: String?, renaming: Bool, scale: CGFloat) {
        // The rename field covers the name while it is edited.
        if nameHidden != renaming {
            nameHidden = renaming
            label.isHidden = renaming
        }
        let old = row
        guard new != old || newPath != path else { return }
        row = new
        if new?.name != old?.name || new?.highlights != old?.highlights {
            if let new, !new.highlights.isEmpty {
                label.attributedStringValue = highlighted(new.name, new.highlights, font: label.font ?? .systemFont(ofSize: NSFont.systemFontSize))
            } else {
                label.stringValue = new?.name ?? ""
            }
        }
        if new?.cut != old?.cut { alphaValue = new?.cut == true ? 0.5 : 1 }
        if new?.tooltip != old?.tooltip { toolTip = new.flatMap { $0.tooltip.isEmpty ? nil : $0.tooltip } }
        guard newPath != path || new?.thumbnail != thumbnail || new?.isDir != old?.isDir else { return }
        path = newPath
        thumbnail = new?.thumbnail ?? ""
        if let ticket { ThumbnailLoader.shared.cancel(ticket) }
        ticket = nil
        guard let new, let newPath else {
            icon.layer?.contents = nil
            return
        }
        let pixels = Int(16 * scale)
        if let image = ThumbnailLoader.shared.cached(new.thumbnail) {
            icon.layer?.contents = image
            return
        }
        icon.layer?.contents = IconCache.icon(for: newPath, isDir: new.isDir, pixels: pixels)
        if !new.thumbnail.isEmpty {
            let file = new.thumbnail
            ticket = ThumbnailLoader.shared.load(file) { [weak self] image in
                guard let self, thumbnail == file, let image else { return }
                icon.layer?.contents = image
            }
        } else if IconCache.needsOwnIcon(newPath, isDir: new.isDir) {
            IconCache.ownIcon(for: newPath, pixels: pixels) { [weak self] image in
                guard let self, path == newPath, thumbnail.isEmpty else { return }
                icon.layer?.contents = image
            }
        }
    }
}

/// `name` with the search's matches in bold on a highlight; the ranges are
/// in bytes of its UTF-8.
@MainActor
func highlighted(_ name: String, _ ranges: [NameRange], font: NSFont, attributes: [NSAttributedString.Key: Any] = [:]) -> NSAttributedString {
    var base = attributes
    base[.font] = font
    let text = NSMutableAttributedString(string: name, attributes: base)
    let utf8 = name.utf8
    let bold = NSFontManager.shared.convert(font, toHaveTrait: .boldFontMask)
    for range in ranges {
        guard range.start < range.end, Int(range.end) <= utf8.count,
              let start = utf8.index(utf8.startIndex, offsetBy: Int(range.start), limitedBy: utf8.endIndex),
              let end = utf8.index(utf8.startIndex, offsetBy: Int(range.end), limitedBy: utf8.endIndex),
              let from = start.samePosition(in: name), let to = end.samePosition(in: name) else { continue }
        text.addAttributes(
            [.font: bold, .backgroundColor: NSColor.systemYellow.withAlphaComponent(0.35)],
            range: NSRange(from..<to, in: name)
        )
    }
    return text
}
