import App
import AppKit

/// The icon grid of a tab's rows, drawn by AppKit: a pool of tile views as
/// big as what fits on screen, each placed from the scroll offset and
/// showing whichever row scrolled into its slot. A tile redraws only when
/// what it shows changes; scrolling moves tiles and draws the row that
/// came into sight. The core owns the selection: a click is sent to it and
/// its answer drawn in the same turn of the run loop.
@MainActor
final class FileGrid: NSObject, FileSurface {
    let scrollView = NSScrollView()
    private let grid: FileGridView

    var keyView: NSView { grid }

    init(owner: FolderController) {
        grid = FileGridView(owner: owner)
        super.init()
        scrollView.documentView = grid
        scrollView.hasVerticalScroller = true
        scrollView.autohidesScrollers = true
        scrollView.drawsBackground = false
        grid.watch(scrollView.contentView)
    }

    func show(_ tab: TabViewModel, config: FileViewConfig) {
        grid.show(tab, iconSize: config.iconSize)
    }

    func nameFrame(of row: UInt32) -> NSRect? {
        grid.nameFrame(of: row)
    }

    func scroll(toTop row: UInt32) {
        grid.scroll(toTop: row)
    }
}

/// The grid's content: as tall as all the rows, holding only the tiles in
/// sight.
final class FileGridView: NSView, NSDraggingSource, NSViewToolTipOwner, NSUserInterfaceValidations {
    private unowned let owner: FolderController
    private var tab: TabViewModel?
    private var iconSize = IconSize.medium
    private(set) var placement = TilePlacement(width: 0, tile: CGSize(width: 96, height: 96), spacing: 12, padding: 12, count: 0)
    /// The pool: tile `index` is drawn by the view at `index % tiles.count`,
    /// so a row scrolling out frees the views the row scrolling in takes.
    private(set) var tiles: [TileView] = []
    private var observers: [NSObjectProtocol] = []
    /// The tile under the pointer, and whether the pointer is over the grid.
    private var hovered: UInt32?
    private var inside = false
    private var hoverCheck: DispatchWorkItem?
    /// The mouse button is down: on which tile and where, whether its click
    /// waits for the button to come up, and whether a drag began.
    private var pressed: (index: UInt32?, point: NSPoint, waiting: Bool, dragged: Bool)?
    /// The folder tile a drop would go into.
    private var dropRow: UInt32?
    /// Plays the one tile whose preview plays.
    private let player = NSImageView()
    private var playing: String?

    init(owner: FolderController) {
        self.owner = owner
        super.init(frame: .zero)
        registerForDraggedTypes([.fileURL])
        player.animates = true
        player.imageScaling = .scaleProportionallyUpOrDown
        player.isHidden = true
        addSubview(player)
    }

    @available(*, unavailable)
    required init?(coder _: NSCoder) { fatalError("made in code") }

    deinit {
        for observer in observers { NotificationCenter.default.removeObserver(observer) }
    }

    override var isFlipped: Bool { true }
    override var acceptsFirstResponder: Bool { true }

    /// Every click is the grid's: tiles only draw.
    override func hitTest(_ point: NSPoint) -> NSView? {
        let found = super.hitTest(point)
        return found is TileView || found === player ? self : found
    }

    override func viewDidMoveToWindow() {
        super.viewDidMoveToWindow()
        if let window, window.firstResponder === window {
            window.makeFirstResponder(self)
        }
        relayout()
    }

    override func viewDidChangeBackingProperties() {
        super.viewDidChangeBackingProperties()
        for tile in tiles { tile.forget() }
        layoutTiles()
    }

    /// Follows the scroll view: its size decides the columns, its offset
    /// the tiles in sight.
    fileprivate func watch(_ clip: NSClipView) {
        clip.postsBoundsChangedNotifications = true
        clip.postsFrameChangedNotifications = true
        observers.append(NotificationCenter.default.addObserver(
            forName: NSView.boundsDidChangeNotification, object: clip, queue: .main
        ) { [weak self] _ in
            MainActor.assumeIsolated { self?.scrolled() }
        })
        observers.append(NotificationCenter.default.addObserver(
            forName: NSView.frameDidChangeNotification, object: clip, queue: .main
        ) { [weak self] _ in
            MainActor.assumeIsolated { self?.relayout() }
        })
    }

    // MARK: Showing the tab

    fileprivate func show(_ new: TabViewModel, iconSize size: IconSize) {
        tab = new
        iconSize = size
        relayout()
    }

    /// The icon's edge for a size, and whether tiles show a second line.
    private var metrics: (icon: CGFloat, detail: Bool) {
        switch iconSize {
        case .small: (40, false)
        case .medium: (56, false)
        case .large: (80, true)
        case .extraLarge: (112, true)
        }
    }

    /// Sizes the grid for its rows and the scroll view's width, then places
    /// the tiles.
    private func relayout() {
        guard let tab, let scroll = enclosingScrollView else { return }
        let clip = scroll.contentView
        let (icon, detail) = metrics
        let tile = CGSize(width: icon + 40, height: icon + 40 + (detail ? 16 : 0))
        let width = clip.bounds.width
        placement = TilePlacement(width: width, tile: tile, spacing: 12, padding: 12, count: tab.totalRows)
        let insets = scroll.contentInsets
        let size = NSSize(width: width, height: max(placement.height, clip.bounds.height - insets.top - insets.bottom))
        if frame.size != size {
            setFrameSize(size)
        }
        layoutTiles()
    }

    private func scrolled() {
        layoutTiles()
        // The content moved under a pointer that stays put: what it is on
        // is looked at again once the scrolling rests, not for every tile
        // that passes under it.
        guard inside else { return }
        hover(nil)
        hoverCheck?.cancel()
        let check = DispatchWorkItem { [weak self] in
            guard let self, inside, let window else { return }
            pointer(at: convert(window.mouseLocationOutsideOfEventStream, from: nil))
        }
        hoverCheck = check
        DispatchQueue.main.asyncAfter(deadline: .now() + 0.15, execute: check)
    }

    /// Gives each tile in sight (and a row more each way) its view, placed
    /// and showing its row.
    private func layoutTiles() {
        guard let tab, let scroll = enclosingScrollView, placement.count == tab.totalRows else { return }
        let clip = scroll.contentView.bounds
        let range = placement.items(from: clip.minY, height: clip.height)
        let pool = Int(placement.pool(height: clip.height))
        if tiles.count != pool { resizePool(pool) }
        guard pool > 0 else { return }
        let (icon, detail) = metrics
        let scale = window?.backingScaleFactor ?? 2
        var used = [Bool](repeating: false, count: pool)
        for index in range {
            let slot = Int(index) % pool
            used[slot] = true
            let tile = tiles[slot]
            let frame = NSRect(origin: placement.origin(of: index), size: placement.tile)
            if tile.frame != frame { tile.frame = frame }
            if tile.isHidden { tile.isHidden = false }
            let row = tab.row(at: index)
            tile.show(
                row, index: index, path: row.map { tab.path(of: $0) },
                selected: tab.isSelected(index), dropTarget: dropRow == index,
                renaming: tab.renaming?.row == index, icon: icon, detail: detail, scale: scale
            )
        }
        for (slot, tile) in tiles.enumerated() where !used[slot] && !tile.isHidden {
            tile.isHidden = true
        }
        let sight = placement.inSight(from: clip.minY, height: clip.height)
        if !sight.isEmpty {
            owner.rowsInSight(first: sight.lowerBound, count: UInt32(sight.count))
        }
        placePlayer(in: range, icon: icon)
    }

    private func resizePool(_ pool: Int) {
        while tiles.count > pool {
            tiles.removeLast().removeFromSuperview()
        }
        while tiles.count < pool {
            let tile = TileView()
            tiles.append(tile)
            addSubview(tile, positioned: .below, relativeTo: player)
        }
    }

    /// The view drawing tile `index`, when it is drawn.
    private func tile(at index: UInt32) -> TileView? {
        guard !tiles.isEmpty else { return nil }
        let tile = tiles[Int(index) % tiles.count]
        return tile.index == index && !tile.isHidden ? tile : nil
    }

    /// Puts the animated preview over the tile that plays, if one does.
    private func placePlayer(in range: Range<UInt32>, icon: CGFloat) {
        guard let tab else { return }
        var file: String?
        var at: UInt32 = 0
        for index in range {
            if let animation = tab.row(at: index)?.animation, !animation.isEmpty {
                file = animation
                at = index
                break
            }
        }
        guard let file else {
            if !player.isHidden {
                player.isHidden = true
                player.image = nil
                playing = nil
            }
            return
        }
        let origin = placement.origin(of: at)
        let frame = NSRect(x: origin.x + (placement.tile.width - icon) / 2, y: origin.y + TileView.top, width: icon, height: icon)
        if player.frame != frame { player.frame = frame }
        guard playing != file else { return }
        playing = file
        // Read off the main thread; the frames decode as they play.
        DispatchQueue.global(qos: .userInitiated).async { [weak self] in
            let data = try? Data(contentsOf: URL(fileURLWithPath: file))
            DispatchQueue.main.async {
                MainActor.assumeIsolated {
                    guard let self, self.playing == file, let data else { return }
                    self.player.image = NSImage(data: data)
                    self.player.isHidden = false
                }
            }
        }
    }

    fileprivate func nameFrame(of row: UInt32) -> NSRect? {
        guard tile(at: row) != nil else { return nil }
        let origin = placement.origin(of: row)
        let (icon, _) = metrics
        return NSRect(x: origin.x, y: origin.y + TileView.top + icon + 4, width: placement.tile.width, height: 22)
    }

    fileprivate func scroll(toTop row: UInt32) {
        guard let scroll = enclosingScrollView else { return }
        let clip = scroll.contentView
        let top = scroll.contentInsets.top
        let bottom = max(-top, frame.height - clip.bounds.height + scroll.contentInsets.bottom)
        let y = min(placement.origin(of: row).y - placement.padding - top, bottom)
        clip.scroll(to: NSPoint(x: clip.bounds.minX, y: y))
        scroll.reflectScrolledClipView(clip)
    }

    /// Scrolls just enough for tile `row` to show whole.
    private func reveal(_ row: UInt32) {
        let frame = NSRect(origin: placement.origin(of: row), size: placement.tile)
        scrollToVisible(frame.insetBy(dx: 0, dy: -placement.padding))
    }

    // MARK: The pointer

    override func updateTrackingAreas() {
        super.updateTrackingAreas()
        for area in trackingAreas { removeTrackingArea(area) }
        addTrackingArea(NSTrackingArea(
            rect: .zero,
            options: [.mouseMoved, .mouseEnteredAndExited, .activeInActiveApp, .inVisibleRect],
            owner: self
        ))
    }

    override func mouseEntered(with event: NSEvent) {
        inside = true
        pointer(at: convert(event.locationInWindow, from: nil))
    }

    override func mouseMoved(with event: NSEvent) {
        inside = true
        pointer(at: convert(event.locationInWindow, from: nil))
    }

    override func mouseExited(with _: NSEvent) {
        inside = false
        hover(nil)
    }

    private func pointer(at point: NSPoint) {
        hover(placement.index(at: point))
    }

    /// The pointer moved onto tile `index`, or off every tile: told to the
    /// core, which plays a tile's preview once the pointer rests on it.
    private func hover(_ index: UInt32?) {
        guard index != hovered, let tab else { return }
        let left = hovered
        hovered = index
        removeAllToolTips()
        if let index, tab.row(at: index) != nil {
            addToolTip(NSRect(origin: placement.origin(of: index), size: placement.tile), owner: self, userData: nil)
            owner.core.update(.hoverRow(tab: tab.id, row: index, inside: true))
        } else if let left, tab.row(at: left) != nil {
            owner.core.update(.hoverRow(tab: tab.id, row: left, inside: false))
        }
    }

    func view(_: NSView, stringForToolTip _: NSView.ToolTipTag, point _: NSPoint, userData _: UnsafeMutableRawPointer?) -> String {
        hovered.flatMap { tab?.row(at: $0)?.tooltip } ?? ""
    }

    override func mouseDown(with event: NSEvent) {
        guard let tab else { return }
        owner.activate()
        window?.makeFirstResponder(self)
        let point = convert(event.locationInWindow, from: nil)
        let index = placement.index(at: point)
        let flags = event.modifierFlags
        if flags.contains(.control) {
            if let menu = owner.menu(forRow: index) {
                NSMenu.popUpContextMenu(menu, with: event, for: self)
            }
            return
        }
        guard let index else {
            pressed = (nil, point, false, false)
            // A click on empty space selects nothing, as in Finder.
            if tab.selectedCount > 0, !flags.contains(.command), !flags.contains(.shift) {
                owner.core.update(.clearSelection(tab: tab.id))
                owner.core.refreshNow()
            }
            return
        }
        if event.clickCount == 2 {
            pressed = nil
            owner.open(row: index, command: flags.contains(.command))
            return
        }
        let command = flags.contains(.command)
        let shift = flags.contains(.shift)
        // A plain click on a selected tile waits for the button to come up:
        // it may be the start of a drag of the whole selection.
        let waiting = !command && !shift && tab.isSelected(index) && tab.selectedCount > 1
        pressed = (index, point, waiting, false)
        if !waiting {
            owner.core.click(tab: tab.id, row: index, command: command, shift: shift)
        }
    }

    override func mouseDragged(with event: NSEvent) {
        guard var press = pressed, let index = press.index, !press.dragged else { return }
        let point = convert(event.locationInWindow, from: nil)
        guard hypot(point.x - press.point.x, point.y - press.point.y) > 4 else { return }
        press.dragged = true
        pressed = press
        beginDrag(of: index, with: event)
    }

    override func mouseUp(with _: NSEvent) {
        defer { pressed = nil }
        guard let press = pressed, let index = press.index, press.waiting, !press.dragged, let tab else { return }
        owner.core.click(tab: tab.id, row: index, command: false, shift: false)
    }

    override func menu(for event: NSEvent) -> NSMenu? {
        owner.menu(forRow: placement.index(at: convert(event.locationInWindow, from: nil)))
    }

    // MARK: Keys

    override func keyDown(with event: NSEvent) {
        if owner.keyDown(event) { return }
        guard let tab, tab.totalRows > 0 else {
            super.keyDown(with: event)
            return
        }
        let columns = Int(placement.columns)
        let total = Int(tab.totalRows)
        let visible = enclosingScrollView?.contentView.bounds.height ?? 0
        let page = max(1, Int(visible / placement.rowHeight)) * columns
        let current = (tab.cursor ?? tab.selection.first?.start).map { Int($0) }
        var target: Int
        switch event.keyCode {
        case 123: target = current.map { $0 - 1 } ?? total - 1 // Left
        case 124: target = current.map { $0 + 1 } ?? 0 // Right
        case 126: target = current.map { $0 >= columns ? $0 - columns : $0 } ?? total - 1 // Up
        case 125: // Down, to the last tile from the row above a short last row
            target = current.map { $0 + columns < total ? $0 + columns : ($0 / columns < (total - 1) / columns ? total - 1 : $0) } ?? 0
        case 115: target = 0 // Home
        case 119: target = total - 1 // End
        case 116: target = current.map { $0 - page } ?? 0 // Page Up
        case 121: target = current.map { $0 + page } ?? total - 1 // Page Down
        default:
            super.keyDown(with: event)
            return
        }
        target = min(max(target, 0), total - 1)
        let shift = event.modifierFlags.contains(.shift)
        owner.core.update(.moveCursor(tab: tab.id, row: UInt32(target), command: false, shift: shift))
        owner.core.refreshNow()
        reveal(UInt32(target))
    }

    override func selectAll(_: Any?) { owner.selectAll() }
    @objc func copy(_: Any?) { owner.copySelection() }
    @objc func cut(_: Any?) { owner.cutSelection() }
    @objc func paste(_: Any?) { owner.pasteItems() }
    @objc func delete(_: Any?) { owner.deleteSelection() }

    func validateUserInterfaceItem(_ item: NSValidatedUserInterfaceItem) -> Bool {
        owner.validate(item.action)
    }

    // MARK: Drag and drop

    /// Drags tile `index`, or the whole selection when it is part of it:
    /// as many of its rows as the core has sent.
    private func beginDrag(of index: UInt32, with event: NSEvent) {
        guard let tab else { return }
        var rows: [UInt32] = [index]
        if tab.isSelected(index) {
            rows = tab.rows.filter(\.selected).map(\.index)
            if !rows.contains(index) { rows.append(index) }
        }
        let fallback = NSRect(origin: placement.origin(of: index), size: placement.tile)
        let items: [NSDraggingItem] = rows.compactMap { row in
            guard let writer = owner.dragItem(row: row) else { return nil }
            let item = NSDraggingItem(pasteboardWriter: writer)
            if let tile = tile(at: row) {
                item.setDraggingFrame(tile.frame, contents: tile.snapshot())
            } else {
                item.setDraggingFrame(fallback, contents: nil)
            }
            return item
        }
        guard !items.isEmpty else { return }
        beginDraggingSession(with: items, event: event, source: self)
    }

    func draggingSession(_: NSDraggingSession, sourceOperationMaskFor context: NSDraggingContext) -> NSDragOperation {
        context == .outsideApplication ? [.copy, .move, .generic, .link] : [.copy, .move, .generic]
    }

    override func draggingEntered(_ sender: NSDraggingInfo) -> NSDragOperation {
        draggingUpdated(sender)
    }

    override func draggingUpdated(_ sender: NSDraggingInfo) -> NSDragOperation {
        let over = placement.index(at: convert(sender.draggingLocation, from: nil))
        let target = owner.dropTarget(sender, row: over)
        setDropRow(target.row)
        owner.showFolderDrop(target.row == nil && !target.operation.isEmpty)
        return target.operation
    }

    override func draggingExited(_: NSDraggingInfo?) {
        setDropRow(nil)
        owner.showFolderDrop(false)
    }

    override func draggingEnded(_: NSDraggingInfo) {
        setDropRow(nil)
        owner.showFolderDrop(false)
    }

    override func performDragOperation(_ sender: NSDraggingInfo) -> Bool {
        let row = dropRow
        setDropRow(nil)
        owner.showFolderDrop(false)
        return owner.drop(sender, row: row)
    }

    private func setDropRow(_ row: UInt32?) {
        guard row != dropRow else { return }
        dropRow = row
        layoutTiles()
    }
}

/// One tile: the file's thumbnail or icon over its name, on the selection's
/// colour when selected. Drawn into its own layer, again only when what it
/// shows changes.
final class TileView: NSView {
    /// From the tile's top to its icon.
    static let top: CGFloat = 4

    private(set) var index = UInt32.max
    private var row: RowView?
    private var path: String?
    private var selected = false
    private var dropTarget = false
    private var renaming = false
    private var icon: CGFloat = 56
    private var detail = false
    private var scale: CGFloat = 2
    private var image: CGImage?
    /// The image is the file's own picture, framed, not an icon.
    private var framed = false
    private var thumbnail = ""
    private var ticket: ThumbnailLoader.Ticket?
    private var name: NSAttributedString?

    init() {
        super.init(frame: .zero)
        wantsLayer = true
        layerContentsRedrawPolicy = .onSetNeedsDisplay
    }

    @available(*, unavailable)
    required init?(coder _: NSCoder) { fatalError("made in code") }

    override var isFlipped: Bool { true }

    override func viewDidChangeEffectiveAppearance() {
        super.viewDidChangeEffectiveAppearance()
        name = nil
        needsDisplay = true
    }

    /// Forgets what it shows, so the next `show` draws and loads again.
    func forget() {
        index = .max
        path = nil
        thumbnail = ""
    }

    func show(
        _ new: RowView?, index newIndex: UInt32, path newPath: String?, selected newSelected: Bool,
        dropTarget newDrop: Bool, renaming newRenaming: Bool, icon newIcon: CGFloat, detail newDetail: Bool,
        scale newScale: CGFloat
    ) {
        if selected != newSelected || dropTarget != newDrop || renaming != newRenaming || icon != newIcon
            || detail != newDetail {
            selected = newSelected
            dropTarget = newDrop
            renaming = newRenaming
            detail = newDetail
            needsDisplay = true
        }
        let old = row
        let sized = icon != newIcon || scale != newScale
        let moved = index != newIndex
        index = newIndex
        icon = newIcon
        scale = newScale
        guard moved || sized || new != old || newPath != path else { return }
        row = new
        if new?.name != old?.name || new?.highlights != old?.highlights { name = nil }
        if (new?.cut == true) != (old?.cut == true) { alphaValue = new?.cut == true ? 0.5 : 1 }
        needsDisplay = true
        guard sized || newPath != path || new?.thumbnail != thumbnail || new?.isDir != old?.isDir else { return }
        path = newPath
        thumbnail = new?.thumbnail ?? ""
        if let ticket { ThumbnailLoader.shared.cancel(ticket) }
        ticket = nil
        guard let new, let newPath else {
            image = nil
            framed = false
            return
        }
        if let cached = ThumbnailLoader.shared.cached(new.thumbnail) {
            image = cached
            framed = true
            return
        }
        let pixels = Int(icon * scale)
        image = IconCache.icon(for: newPath, isDir: new.isDir, pixels: pixels)
        framed = false
        if !new.thumbnail.isEmpty {
            let file = new.thumbnail
            ticket = ThumbnailLoader.shared.load(file) { [weak self] loaded in
                guard let self, thumbnail == file, let loaded else { return }
                image = loaded
                framed = true
                needsDisplay = true
            }
        } else if IconCache.needsOwnIcon(newPath, isDir: new.isDir) {
            IconCache.ownIcon(for: newPath, pixels: pixels) { [weak self] own in
                guard let self, path == newPath, thumbnail.isEmpty else { return }
                image = own
                needsDisplay = true
            }
        }
    }

    /// The tile as drawn, for dragging it.
    func snapshot() -> NSImage? {
        guard let bitmap = bitmapImageRepForCachingDisplay(in: bounds) else { return nil }
        cacheDisplay(in: bounds, to: bitmap)
        let image = NSImage(size: bounds.size)
        image.addRepresentation(bitmap)
        return image
    }

    override func draw(_: NSRect) {
        if selected {
            NSColor.controlAccentColor.withAlphaComponent(0.25).setFill()
            NSBezierPath(roundedRect: bounds, xRadius: 8, yRadius: 8).fill()
        }
        if dropTarget {
            NSColor.controlAccentColor.setStroke()
            let outline = NSBezierPath(roundedRect: bounds.insetBy(dx: 1, dy: 1), xRadius: 8, yRadius: 8)
            outline.lineWidth = 2
            outline.stroke()
        }
        guard let row else { return }
        let box = NSRect(x: (bounds.width - icon) / 2, y: Self.top, width: icon, height: icon)
        if let image, let context = NSGraphicsContext.current?.cgContext {
            var rect = box
            if framed, image.width > 0, image.height > 0 {
                // As large as fits the box, in the picture's own shape.
                let fit = min(box.width / CGFloat(image.width), box.height / CGFloat(image.height))
                let size = CGSize(width: CGFloat(image.width) * fit, height: CGFloat(image.height) * fit)
                rect = NSRect(x: box.midX - size.width / 2, y: box.midY - size.height / 2, width: size.width, height: size.height)
            }
            context.saveGState()
            if framed {
                context.setShadow(offset: CGSize(width: 0, height: -1), blur: 1, color: NSColor.black.withAlphaComponent(0.2).cgColor)
            }
            // The view is flipped; the picture isn't.
            context.translateBy(x: 0, y: rect.minY + rect.maxY)
            context.scaleBy(x: 1, y: -1)
            context.interpolationQuality = .high
            context.draw(image, in: rect)
            context.restoreGState()
            if framed {
                NSColor.labelColor.withAlphaComponent(0.15).setStroke()
                NSBezierPath(rect: rect.insetBy(dx: 0.5, dy: 0.5)).stroke()
            }
        }
        let text = NSRect(x: 2, y: Self.top + icon + 4, width: bounds.width - 4, height: 32)
        if !renaming {
            if name == nil {
                name = highlighted(row.name, row.highlights, font: Self.nameFont, attributes: Self.nameAttributes)
            }
            name?.draw(with: text, options: [.usesLineFragmentOrigin, .truncatesLastVisibleLine])
        }
        // Large tiles add a line, as egui's: the size, or a search result's
        // folder.
        if detail {
            let line = row.folder.isEmpty ? row.detail : row.folder
            (line as NSString).draw(
                with: NSRect(x: 2, y: text.maxY, width: bounds.width - 4, height: 14),
                options: [.usesLineFragmentOrigin, .truncatesLastVisibleLine], attributes: Self.detailAttributes
            )
        }
    }

    private static let nameFont = NSFont.preferredFont(forTextStyle: .callout)

    private static let nameAttributes: [NSAttributedString.Key: Any] = {
        let paragraph = NSMutableParagraphStyle()
        paragraph.alignment = .center
        paragraph.lineBreakMode = .byWordWrapping
        return [.foregroundColor: NSColor.labelColor, .paragraphStyle: paragraph]
    }()

    private static let detailAttributes: [NSAttributedString.Key: Any] = {
        let paragraph = NSMutableParagraphStyle()
        paragraph.alignment = .center
        paragraph.lineBreakMode = .byTruncatingHead
        return [
            .font: NSFont.preferredFont(forTextStyle: .caption1),
            .foregroundColor: NSColor.secondaryLabelColor,
            .paragraphStyle: paragraph,
        ]
    }()

    // For VoiceOver: a tile is its file.
    override func isAccessibilityElement() -> Bool { row != nil }
    override func accessibilityRole() -> NSAccessibility.Role? { .image }
    override func accessibilityLabel() -> String? { row?.name }
    override func isAccessibilitySelected() -> Bool { selected }
}

/// Where tiles go for a width: columns spread over it, as an adaptive
/// `GridItem` spreads them.
struct TilePlacement {
    let columns: UInt32
    let columnWidth: CGFloat
    let rowHeight: CGFloat
    let tile: CGSize
    let spacing: CGFloat
    let padding: CGFloat
    let count: UInt32

    init(width: CGFloat, tile: CGSize, spacing: CGFloat, padding: CGFloat, count: UInt32) {
        let usable = max(tile.width, width - 2 * padding)
        columns = max(1, UInt32((usable + spacing) / (tile.width + spacing)))
        columnWidth = (usable - CGFloat(columns - 1) * spacing) / CGFloat(columns)
        rowHeight = tile.height + spacing
        self.tile = tile
        self.spacing = spacing
        self.padding = padding
        self.count = count
    }

    var rows: UInt32 { (count + columns - 1) / columns }

    var height: CGFloat { 2 * padding + max(0, CGFloat(rows) * rowHeight - spacing) }

    func origin(of index: UInt32) -> CGPoint {
        let column = index % columns
        let row = index / columns
        return CGPoint(
            x: padding + CGFloat(column) * (columnWidth + spacing) + (columnWidth - tile.width) / 2,
            y: padding + CGFloat(row) * rowHeight
        )
    }

    /// The tile at `point` in the content, if any.
    func index(at point: CGPoint) -> UInt32? {
        guard point.y >= padding, point.x >= padding else { return nil }
        let row = UInt32((point.y - padding) / rowHeight)
        let column = UInt32((point.x - padding) / (columnWidth + spacing))
        guard column < columns else { return nil }
        let index = row * columns + column
        let origin = origin(of: index)
        guard index < count, point.x < origin.x + tile.width, point.x >= origin.x,
              point.y < origin.y + tile.height else { return nil }
        return index
    }

    /// The tiles at least partly in sight.
    func inSight(from offset: CGFloat, height: CGFloat) -> Range<UInt32> {
        let first = UInt32(max(0, (offset - padding) / rowHeight))
        let last = UInt32(max(0, (offset + height - padding) / rowHeight)) + 1
        return min(count, first * columns) ..< min(count, last * columns)
    }

    /// The tiles to draw: always `pool` of them, from the row above
    /// the first in sight, so the row scrolling out frees just the
    /// slots the row scrolling in takes.
    func items(from offset: CGFloat, height: CGFloat) -> Range<UInt32> {
        let poolRows = pool(height: height) / columns
        let first = UInt32(max(0, (offset - padding) / rowHeight))
        let start = min(first > 0 ? first - 1 : 0, rows > poolRows ? rows - poolRows : 0)
        return min(count, start * columns) ..< min(count, (start + poolRows) * columns)
    }

    /// How many tiles are drawn for a height: the rows in sight, part
    /// rows at both ends and a row more each way.
    func pool(height: CGFloat) -> UInt32 {
        (UInt32((max(0, height) / rowHeight).rounded(.up)) + 3) * columns
    }
}
