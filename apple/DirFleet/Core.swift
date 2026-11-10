import App
import AppKit
import Foundation
import Observation
import os
import Shared
import UniformTypeIdentifiers

/// Receives effects from Rust threads.
private final class EffectHandler: CruxShell, @unchecked Sendable {
    var handler: ((Data) -> Void)?

    func processEffects(bytes: Data) {
        handler?(bytes)
    }
}

/// Asks the user for a folder or an app. Swapped out in tests.
@MainActor
protocol FolderPicker {
    func pick(title: String) async -> String?
    func pickApp(title: String) async -> String?
}

extension FolderPicker {
    func pickApp(title _: String) async -> String? { nil }
}

/// The system open panel.
struct OpenPanelPicker: FolderPicker {
    func pick(title: String) async -> String? {
        let panel = NSOpenPanel()
        panel.message = title
        panel.prompt = title
        panel.canChooseFiles = false
        panel.canChooseDirectories = true
        panel.allowsMultipleSelection = false
        guard await panel.begin() == .OK else { return nil }
        return panel.url?.path
    }

    func pickApp(title: String) async -> String? {
        let panel = NSOpenPanel()
        panel.message = title
        panel.prompt = "Open"
        panel.allowedContentTypes = [.application]
        panel.directoryURL = URL(fileURLWithPath: "/Applications")
        panel.allowsMultipleSelection = false
        guard await panel.begin() == .OK else { return nil }
        return panel.url?.path
    }
}

/// Bytes of a row's name the search matched. SwiftUI has a `TextRange` and
/// an `App` of its own, so views name it this way.
typealias NameRange = App.TextRange
/// The core's settings; SwiftUI has a `Settings` scene.
typealias AppSettings = App.Settings
/// How a folder is shown; the shell's own `FolderView` draws one.
typealias FolderLook = App.FolderView
/// The palette's state; the shell's own palette view draws it.
typealias PaletteModel = App.PaletteView

/// A request to show a tab's search field; each one differs from the last.
struct FindRequest: Equatable {
    let tab: UInt32
    let count: Int
}

/// What the bridge did, for performance tests: counts, and seconds spent on
/// the main thread.
struct BridgeStats {
    /// Effect batches from the core, and the renders among them.
    var batches = 0
    var renders = 0
    /// View models fetched and decoded, and their bytes.
    var refreshes = 0
    /// Refreshes whose view equalled the last one.
    var unchanged = 0
    var bytes = 0
    var viewSeconds = 0.0
    var decodeSeconds = 0.0
    /// The last viewport reported: tab, first row, rows.
    var viewport: (UInt32, UInt32, UInt32)?
    /// Events sent from the main thread.
    var updates = 0
    var updateSeconds = 0.0
}

/// The core's view model with one observed property per part, so a view
/// reading `settings` isn't drawn again when only a tab changed. Each
/// refresh assigns only the parts that differ. Windows and their tabs have
/// stores of their own, with their own parts.
@MainActor
@Observable
final class ViewStore {
    /// The windows as last fetched, whole. Not observed: for handlers and
    /// for menus when they act, never for what a view draws.
    @ObservationIgnored private(set) var snapshot: [WindowView]
    /// A store per window and per shown tab; these change only when a
    /// window or a tab comes or goes.
    private(set) var windowStores: [UInt32: WindowStore]
    private(set) var tabs: [UInt32: TabStore]
    /// The window that shows the app-wide dialogs.
    private(set) var firstWindow: UInt32?
    /// The favorite folders' paths, and whether the sidebar lists favorites.
    private(set) var favorites: [String]
    private(set) var hasFavorites: Bool
    private(set) var sorts: [SortChoice]
    private(set) var columns: [ColumnView]
    private(set) var optionalColumns: [ColumnChoice]
    private(set) var trash: TrashInfo
    private(set) var searchFilters: [FilterGroup]
    private(set) var toasts: [ToastView]
    private(set) var inspectorShown: Bool
    private(set) var settings: Settings
    private(set) var commands: [CommandView]
    private(set) var palette: PaletteView?
    private(set) var savedSearches: [SavedSearchView]
    private(set) var iconSizes: [IconSizeChoice]
    private(set) var folderDefaults: App.FolderView
    private(set) var operations: [OperationView]
    private(set) var activity: [ActivityView]
    private(set) var ejectConfirm: EjectConfirmView?
    private(set) var searchHelp: [SearchHelpRow]
    private(set) var access: AccessView
    private(set) var cutCount: UInt32
    private(set) var thumbnailPicker: ThumbnailPickerView?
    private(set) var plugins: PluginsView
    private(set) var pluginForm: PluginFormView?
    private(set) var pluginJobs: [PluginJobView]
    private(set) var pluginShortcuts: [PluginShortcutView]
    private(set) var pluginResults: PluginResultsView?
    private(set) var undo: String?
    private(set) var redo: String?
    private(set) var conflict: ConflictView?

    init(_ model: ViewModel) {
        snapshot = model.windows
        windowStores = [:]
        tabs = [:]
        firstWindow = nil
        favorites = []
        hasFavorites = false
        sorts = model.sorts
        columns = model.columns
        optionalColumns = model.optionalColumns
        trash = model.trash
        searchFilters = model.searchFilters
        toasts = model.toasts
        inspectorShown = model.inspectorShown
        settings = model.settings
        commands = model.commands
        palette = model.palette
        savedSearches = model.savedSearches
        iconSizes = model.iconSizes
        folderDefaults = model.folderDefaults
        operations = model.operations
        activity = model.activity
        ejectConfirm = model.ejectConfirm
        searchHelp = model.searchHelp
        access = model.access
        cutCount = model.cutCount
        thumbnailPicker = model.thumbnailPicker
        plugins = model.plugins
        pluginForm = model.pluginForm
        pluginJobs = model.pluginJobs
        pluginShortcuts = model.pluginShortcuts
        pluginResults = model.pluginResults
        undo = model.undo
        redo = model.redo
        conflict = model.conflict
        setWindows(model.windows)
    }

    /// Hands each window and shown tab its part of `windows`.
    private func setWindows(_ windows: [WindowView]) {
        snapshot = windows
        var shownWindows: [UInt32: WindowStore] = [:]
        var shownTabs: [UInt32: TabStore] = [:]
        for window in windows {
            if let store = windowStores[window.id] {
                store.update(window)
                shownWindows[window.id] = store
            } else {
                shownWindows[window.id] = WindowStore(window)
            }
            for tab in window.shownTabs {
                if let store = tabs[tab.id] {
                    store.update(tab)
                    shownTabs[tab.id] = store
                } else {
                    shownTabs[tab.id] = TabStore(tab)
                }
            }
        }
        if windowStores.count != shownWindows.count || !shownWindows.keys.allSatisfy({ windowStores[$0] != nil }) {
            windowStores = shownWindows
        }
        if tabs.count != shownTabs.count || !shownTabs.keys.allSatisfy({ tabs[$0] != nil }) {
            tabs = shownTabs
        }
        if firstWindow != windows.first?.id { firstWindow = windows.first?.id }
        let section = windows.first?.sidebar.first { $0.section == .favorites }
        let paths = section?.items.map(\.path) ?? []
        if favorites != paths { favorites = paths }
        if hasFavorites != (section != nil) { hasFavorites = section != nil }
    }

    /// The shown tab `id` without its rows and selection, for menus.
    func shown(_ id: UInt32?) -> TabViewModel? {
        id.flatMap { tabs[$0] }?.chrome
    }

    /// How much of the shown tab `id` is selected.
    func selected(_ id: UInt32?) -> SelectionKind {
        id.flatMap { tabs[$0] }?.selected ?? .none
    }

    func update(_ model: ViewModel) {
        setWindows(model.windows)
        if sorts != model.sorts { sorts = model.sorts }
        if columns != model.columns { columns = model.columns }
        if optionalColumns != model.optionalColumns { optionalColumns = model.optionalColumns }
        if trash != model.trash { trash = model.trash }
        if searchFilters != model.searchFilters { searchFilters = model.searchFilters }
        if toasts != model.toasts { toasts = model.toasts }
        if inspectorShown != model.inspectorShown { inspectorShown = model.inspectorShown }
        if settings != model.settings { settings = model.settings }
        if commands != model.commands { commands = model.commands }
        if palette != model.palette { palette = model.palette }
        if savedSearches != model.savedSearches { savedSearches = model.savedSearches }
        if iconSizes != model.iconSizes { iconSizes = model.iconSizes }
        if folderDefaults != model.folderDefaults { folderDefaults = model.folderDefaults }
        if operations != model.operations { operations = model.operations }
        if activity != model.activity { activity = model.activity }
        if ejectConfirm != model.ejectConfirm { ejectConfirm = model.ejectConfirm }
        if searchHelp != model.searchHelp { searchHelp = model.searchHelp }
        if access != model.access { access = model.access }
        if cutCount != model.cutCount { cutCount = model.cutCount }
        if thumbnailPicker != model.thumbnailPicker { thumbnailPicker = model.thumbnailPicker }
        if plugins != model.plugins { plugins = model.plugins }
        if pluginForm != model.pluginForm { pluginForm = model.pluginForm }
        if pluginJobs != model.pluginJobs { pluginJobs = model.pluginJobs }
        if pluginShortcuts != model.pluginShortcuts { pluginShortcuts = model.pluginShortcuts }
        if pluginResults != model.pluginResults { pluginResults = model.pluginResults }
        if undo != model.undo { undo = model.undo }
        if redo != model.redo { redo = model.redo }
        if conflict != model.conflict { conflict = model.conflict }
    }
}

/// Points of Interest in Instruments: each refresh and event.
private let signposter = OSSignposter(subsystem: "io.github.leinnan.dirfleet", category: .pointsOfInterest)

/// The shared Rust core. File system effects are handled in Rust; only the
/// ones needing the UI thread or native UI reach this class.
@MainActor
@Observable
final class Core {
    let view: ViewStore
    @ObservationIgnored private(set) var stats = BridgeStats()
    /// The bytes `view` was decoded from: an equal view changes nothing.
    @ObservationIgnored private var viewBytes = Data()
    /// When the view was last fetched, and whether a fetch is due.
    @ObservationIgnored private var lastRefresh = ContinuousClock.now - .seconds(1)
    @ObservationIgnored private var refreshDue = false

    @ObservationIgnored private let handler: EffectHandler
    @ObservationIgnored private let core: CoreFfi
    @ObservationIgnored private let picker: FolderPicker
    /// Windows waiting for the core to name them, oldest first.
    @ObservationIgnored private var adopters: [(UInt32) -> Void] = []
    /// Shows a window for a core window; set by the scene.
    @ObservationIgnored var openWindow: ((UInt32) -> Void)?
    /// Whether `start` was called.
    @ObservationIgnored private(set) var started = false
    /// Each core window's NSWindow, once shown.
    @ObservationIgnored private var nsWindows: [UInt32: WeakWindow] = [:]
    /// Where windows' moves and resizes are watched.
    @ObservationIgnored private var frameObservers: [NSObjectProtocol] = []
    /// The window whose native tab group the next opened window joins.
    @ObservationIgnored private var nextTabOf: UInt32?
    /// Windows that join a tab group once shown, and the window to join.
    @ObservationIgnored private var joining: [UInt32: UInt32] = [:]
    /// What Quick Look shows, for the tab `previewTab`; nil when closed.
    var previewURL: URL?
    private(set) var previewItems: [URL] = []
    private(set) var previewTab: UInt32?
    /// The saved search being edited in a sheet.
    var editingSavedSearch: UInt64?
    /// The window the command palette opens in.
    var paletteWindow: UInt32?
    var showingShortcuts = false
    /// The tab whose search field Find (⌘F) last asked to show.
    private(set) var findRequest: FindRequest?

    /// `picker` defaults to the system open panel. Without a `stateFile`
    /// nothing is kept between launches.
    init(picker: FolderPicker? = nil, stateFile: String? = nil) {
        LaunchTime.mark("core_init")
        self.picker = picker ?? OpenPanelPicker()
        handler = EffectHandler()
        if let stateFile {
            core = CoreFfi(withStateFile: handler, stateFile: stateFile)
        } else {
            core = CoreFfi(shell: handler)
        }
        viewBytes = core.view()
        // swiftlint:disable:next force_try
        view = ViewStore(try! .bincodeDeserialize(input: [UInt8](viewBytes)))

        handler.handler = { [weak self] bytes in
            // swiftlint:disable:next force_try
            let requests = try! Requests.bincodeDeserialize(input: [UInt8](bytes)).value
            DispatchQueue.main.async { self?.handle(requests) }
        }
        LaunchTime.mark("core_made")
    }

    /// Restores the saved windows, or opens one at `home`; `open` replaces
    /// them. The window calling this takes the first one: `assign` gets its id.
    func start(open: String?, home: String, _ assign: @escaping (UInt32) -> Void) {
        LaunchTime.mark("window_appeared")
        started = true
        adopters.append(assign)
        update(.started(open: open, home: home))
    }

    /// Gives a window SwiftUI opened by itself (at launch, or restored) a new
    /// core window showing `path`; `assign` gets its id.
    func adopt(path: String?, _ assign: @escaping (UInt32) -> Void) {
        adopters.append(assign)
        update(.newWindow(path: path))
    }

    /// The window `id` as last fetched. Not observed: views read
    /// `view.windowStores` and `view.tabs`.
    func window(_ id: UInt32?) -> WindowView? {
        view.snapshot.first { $0.id == id }
    }

    /// A new window for `path` as a native tab of `window`.
    func newTab(of window: UInt32, path: String?) {
        nextTabOf = window
        update(.newWindow(path: path))
    }

    /// The closed tab as a native tab of `window`.
    func reopenClosedTab(of window: UInt32) {
        nextTabOf = window
        update(.reopenClosedAsWindow)
    }

    /// Runs a saved search in a new native tab of `window`.
    func runSavedSearchInNewTab(of window: UInt32, from tab: UInt32, id: UInt64) {
        nextTabOf = window
        update(.runSavedSearchInNewWindow(tab: tab, id: id))
    }

    /// Remembers the window's NSWindow and puts it in its tab group: the
    /// one asked for, or the one it was in when saved. Its saved frame comes
    /// back, moves and resizes go to the core to be kept, and so does the
    /// window coming to the front.
    func register(window id: UInt32, nsWindow: NSWindow) {
        let known = nsWindows[id]?.window === nsWindow
        nsWindows[id] = WeakWindow(window: nsWindow)
        if !known {
            if let frame = self.window(id)?.frame {
                let rect = NSRect(x: Double(frame.x), y: Double(frame.y),
                                  width: Double(frame.width), height: Double(frame.height))
                // A screen may have gone since: keep the window on one.
                nsWindow.setFrame(nsWindow.constrainFrameRect(rect, to: nsWindow.screen ?? NSScreen.main), display: false)
            }
            for name in [NSWindow.didMoveNotification, NSWindow.didEndLiveResizeNotification] {
                frameObservers.append(NotificationCenter.default.addObserver(
                    forName: name, object: nsWindow, queue: .main
                ) { [weak self] note in
                    guard let moved = note.object as? NSWindow else { return }
                    let frame = moved.frame
                    MainActor.assumeIsolated {
                        self?.update(.windowFramed(window: id, frame: WindowFrame(
                            x: Int32(frame.origin.x), y: Int32(frame.origin.y),
                            width: UInt32(max(frame.width, 0)), height: UInt32(max(frame.height, 0))
                        )))
                    }
                })
            }
            // Other apps may have opened files or replaced the clipboard.
            frameObservers.append(NotificationCenter.default.addObserver(
                forName: NSWindow.didBecomeKeyNotification, object: nsWindow, queue: .main
            ) { [weak self] _ in
                MainActor.assumeIsolated { self?.update(.windowFocused(window: id)) }
            })
        }
        let group = self.window(id)?.group
        let parent = joining.removeValue(forKey: id) ?? group.flatMap { group in
            view.snapshot.first { $0.id != id && $0.group == group && nsWindows[$0.id]?.window != nil }?.id
        }
        guard let parent, let parentWindow = nsWindows[parent]?.window, parentWindow !== nsWindow else { return }
        parentWindow.addTabbedWindow(nsWindow, ordered: .above)
        nsWindow.makeKeyAndOrderFront(nil)
    }

    /// Tells the core which windows share a native tab group, named by the
    /// lowest window id in it, so restoring brings the grouping back.
    func reportGroups() {
        let ids = nsWindows.compactMapValues(\.window)
        for (id, window) in ids {
            let group = window.tabbedWindows.flatMap { tabbed in
                ids.filter { _, other in tabbed.contains(other) }.keys.min()
            }
            update(.windowGrouped(window: id, group: group))
        }
    }

    /// Opens Quick Look on the tab's selection, or closes it when open.
    func toggleQuickLook(tab: UInt32) {
        if previewURL != nil {
            previewURL = nil
        } else {
            update(.quickLook(tab: tab))
        }
    }

    /// Closes Quick Look and opens the item it showed; false when closed.
    func openPreviewed() -> Bool {
        guard let url = previewURL, let tab = previewTab else { return false }
        previewURL = nil
        update(.openPreviewed(tab: tab, path: url.path))
        return true
    }

    /// Shows the search field for `tab`.
    func find(tab: UInt32) {
        findRequest = FindRequest(tab: tab, count: (findRequest?.count ?? 0) + 1)
    }

    /// Deletes everything in the Trash for good, once the user agrees.
    func emptyTrash() {
        if confirm(
            "Empty the \(view.trash.label)?",
            detail: "Every item in it is deleted for good. This cannot be undone.",
            button: "Empty \(view.trash.label)"
        ) {
            update(.emptyTrash)
        }
    }

    /// Deletes the Trash rows selected in `tab` for good, once the user agrees.
    func purgeSelection(tab id: UInt32) {
        guard let tab = tab(id), tab.selectedCount > 0 else { return }
        let items = tab.selectedCount == 1 ? "this item" : "these \(tab.selectedCount) items"
        if confirm(
            "Delete \(items) immediately?",
            detail: "This cannot be undone.",
            button: "Delete"
        ) {
            // The Trash's items are purged; a folder's are deleted for good.
            update(tab.inTrash ? .purgeSelection(tab: id) : .deleteSelection(tab: id))
        }
    }

    /// Files dropped on `target` (the tab's folder when `nil`), Finder's
    /// keys deciding: ⌥ copies, ⌘ moves. Asks first when the settings say so.
    @discardableResult
    func dropItems(tab: UInt32, urls: [URL], target: String?) -> Bool {
        let paths = urls.filter(\.isFileURL).map(\.path)
        guard !paths.isEmpty else { return false }
        let keys = NSEvent.modifierFlags
        var copy = keys.contains(.option)
        var move = keys.contains(.command)
        let isApp = target?.hasSuffix(".app") ?? false
        if view.settings.confirmDrop && !copy && !move && !isApp {
            let alert = NSAlert()
            let items = paths.count == 1 ? "this item" : "these \(paths.count) items"
            alert.messageText = "Move or copy \(items)?"
            alert.addButton(withTitle: "Move")
            alert.addButton(withTitle: "Copy")
            alert.addButton(withTitle: "Cancel")
            switch alert.runModal() {
            case .alertFirstButtonReturn: move = true
            case .alertSecondButtonReturn: copy = true
            default: return false
            }
        }
        update(.dropItems(tab: tab, paths: paths, target: target, copy: copy, forceMove: move))
        return true
    }

    /// The selected folder's path, when exactly one folder is selected.
    func selectedFolder(tab id: UInt32) -> String? {
        guard let tab = tab(id), tab.selectedCount == 1,
              let row = tab.rows.first(where: \.selected), row.isDir else { return nil }
        let folder = row.folder.hasPrefix("/") ? row.folder : (tab.path as NSString).appendingPathComponent(row.folder)
        return (folder as NSString).appendingPathComponent(row.name)
    }

    /// Closes the key window's other native tabs.
    func closeOtherTabs() {
        guard let key = NSApp.keyWindow else { return }
        for other in key.tabbedWindows ?? [] where other != key {
            other.close()
        }
    }

    /// Shows the key window's tab `number` (1 to 8); 9 is the last, as in
    /// Safari and Finder.
    static func showTab(_ number: Int) {
        guard let tabs = NSApp.keyWindow?.tabbedWindows, !tabs.isEmpty else { return }
        let index = number == 9 ? tabs.count - 1 : number - 1
        if index < tabs.count { tabs[index].makeKeyAndOrderFront(nil) }
    }

    /// Forgets every opened file, once the user agrees.
    func clearOpenHistory() {
        if confirm(
            "Clear the open history?",
            detail: "Date opened and Opens start over for every file.",
            button: "Clear"
        ) {
            update(.clearOpenHistory)
        }
    }

    /// Runs a plugin action on the tab's selection, asking first when the
    /// plugin says to.
    func runPluginAction(_ action: PluginActionView, tab: UInt32) {
        if !action.confirm.isEmpty, !confirm(action.title, detail: action.confirm, button: "Continue") {
            return
        }
        update(.runPluginAction(tab: tab, action: action.id))
    }

    /// Every video goes back to its default thumbnail, once the user agrees.
    func resetAllThumbnails() {
        if confirm(
            "Reset all chosen thumbnails?",
            detail: "Every video shows its default frame again.",
            button: "Reset"
        ) {
            update(.resetAllThumbnails)
        }
    }

    /// Asks for a name and saves the tab's search under it.
    func saveSearch(tab id: UInt32) {
        guard let search = tab(id)?.search else { return }
        let alert = NSAlert()
        alert.messageText = "Save Search"
        alert.informativeText = "The search is kept in the sidebar under this name."
        let field = NSTextField(frame: NSRect(x: 0, y: 0, width: 260, height: 24))
        field.stringValue = search.text.isEmpty ? (search.chips.first?.token ?? "Search") : search.text
        alert.accessoryView = field
        alert.addButton(withTitle: "Save")
        alert.addButton(withTitle: "Cancel")
        alert.window.initialFirstResponder = field
        if alert.runModal() == .alertFirstButtonReturn {
            update(.saveSearch(tab: id, name: field.stringValue))
        }
    }

    /// Moves the tab's selection to the Trash, asking first when the
    /// settings say so.
    func trashSelection(tab id: UInt32) {
        guard let tab = tab(id), tab.selectedCount > 0, !tab.inTrash else { return }
        if view.settings.confirmTrash {
            let items = tab.selectedCount == 1 ? "this item" : "these \(tab.selectedCount) items"
            guard confirm(
                "Move \(items) to the \(view.trash.label)?",
                detail: "You can take them back out of the \(view.trash.label).",
                button: "Move to \(view.trash.label)"
            ) else { return }
        }
        update(.trashSelection(tab: id))
    }

    /// Copies or moves the tab's selection into a folder the user picks.
    func sendSelection(tab: UInt32, copy: Bool) {
        Task {
            guard let folder = await picker.pick(title: copy ? "Copy to" : "Move to") else { return }
            update(copy ? .copyTo(tab: tab, folder: folder) : .moveTo(tab: tab, folder: folder))
        }
    }

    /// Undo or Redo: text being edited first, file operations otherwise.
    func undo(redo: Bool) {
        if let window = NSApp.keyWindow, window.firstResponder is NSText {
            NSApp.sendAction(Selector(redo ? "redo:" : "undo:"), to: nil, from: nil)
        } else {
            update(redo ? .redo : .undo)
        }
    }

    private func confirm(_ message: String, detail: String, button: String) -> Bool {
        let alert = NSAlert()
        alert.messageText = message
        alert.informativeText = detail
        alert.alertStyle = .warning
        alert.addButton(withTitle: button).hasDestructiveAction = true
        alert.addButton(withTitle: "Cancel")
        return alert.runModal() == .alertFirstButtonReturn
    }

    /// The shown tab `id` as last fetched, if a window shows it. Not
    /// observed: views read `view.tabs`.
    func tab(_ id: UInt32?) -> TabViewModel? {
        for window in view.snapshot {
            if window.active.id == id { return window.active }
            if let pane = window.panes.first(where: { $0.shown.id == id }) { return pane.shown }
        }
        return nil
    }

    /// The window showing the tab `id`.
    func window(showing id: UInt32) -> WindowView? {
        view.snapshot.first { window in window.active.id == id || window.panes.contains { $0.shown.id == id } }
    }

    /// A click on a row. The selection is fetched at once, so it shows in
    /// this turn of the run loop and not a frame later.
    func click(tab: UInt32, row: UInt32, command: Bool, shift: Bool) {
        update(.click(tab: tab, row: row, command: command, shift: shift))
        refreshNow()
    }

    /// Fetches the view now, for input whose result should show at once.
    func refreshNow() {
        refresh()
    }

    func update(_ event: Event) {
        let state = signposter.beginInterval("update")
        let start = ContinuousClock.now
        // swiftlint:disable:next force_try
        _ = core.update(data: Data(try! event.bincodeSerialize()))
        stats.updates += 1
        stats.updateSeconds += (ContinuousClock.now - start).seconds
        if case let .viewportChanged(tab, firstRow, rows) = event {
            stats.viewport = (tab, firstRow, rows)
        }
        signposter.endInterval("update", state)
    }

    private func handle(_ requests: [Request]) {
        var render = false
        var windows: [UInt32] = []
        stats.batches += 1
        for request in requests {
            switch request.effect {
            case .render:
                stats.renders += 1
                render = true
            case let .pickFolder(operation):
                Task {
                    let path = await picker.pick(title: operation.title)
                    resolve(request.id, path.map(PickFolderOutput.picked) ?? .cancelled)
                }
            case .quitApp:
                // The core set up a relaunch first, which waits for this.
                NSApp.terminate(nil)
            case let .pickApp(operation):
                Task {
                    let path = await picker.pickApp(title: operation.title)
                    resolve(request.id, path.map(PickFolderOutput.picked) ?? .cancelled)
                }
            case let .openWindow(operation):
                windows.append(operation.window)
                if let parent = nextTabOf {
                    joining[operation.window] = parent
                    nextTabOf = nil
                }
            case let .quickLook(operation):
                previewItems = operation.paths.map { URL(fileURLWithPath: $0) }
                previewTab = operation.tab
                let index = Int(operation.index)
                previewURL = index < previewItems.count ? previewItems[index] : previewItems.first
            }
        }
        // Also before showing windows, so each finds its part of the view: the
        // core already holds them, but their render may come in a later call.
        if !windows.isEmpty {
            refresh()
        } else if render {
            scheduleRefresh()
        }
        for window in windows {
            LaunchTime.mark("window_named")
            if adopters.isEmpty {
                openWindow?(window)
            } else {
                adopters.removeFirst()(window)
            }
        }
    }

    private func resolve(_ id: UInt32, _ output: PickFolderOutput) {
        // swiftlint:disable:next force_try
        _ = core.resolve(effectId: id, data: Data(try! output.bincodeSerialize()))
    }

    /// Fetches the view now, or once a frame has passed since the last
    /// fetch: renders from many threads (thumbnails, listings, progress) then
    /// cost one update a frame.
    private func scheduleRefresh() {
        guard !refreshDue else { return }
        let wait = lastRefresh + Self.frame - .now
        if wait <= .zero {
            refresh()
            return
        }
        refreshDue = true
        DispatchQueue.main.asyncAfter(deadline: .now() + wait.seconds) { [weak self] in
            guard let self, refreshDue else { return }
            refresh()
        }
    }

    /// The shortest time between two fetches: one frame at 120 Hz.
    private static let frame = Duration.milliseconds(8)

    private func refresh() {
        let state = signposter.beginInterval("refresh")
        let start = ContinuousClock.now
        refreshDue = false
        lastRefresh = start
        let bytes = core.view()
        let fetched = ContinuousClock.now
        stats.refreshes += 1
        stats.bytes += bytes.count
        stats.viewSeconds += (fetched - start).seconds
        // The same view again (a render for rows not shown, a drive poll that
        // changed nothing shown) leaves SwiftUI alone.
        if bytes != viewBytes {
            viewBytes = bytes
            // swiftlint:disable:next force_try
            view.update(try! .bincodeDeserialize(input: [UInt8](bytes)))
            stats.decodeSeconds += (ContinuousClock.now - fetched).seconds
        } else {
            stats.unchanged += 1
        }
        signposter.endInterval("refresh", state, "\(bytes.count) bytes")
    }
}

/// An NSWindow the core doesn't keep alive.
private struct WeakWindow {
    weak var window: NSWindow?
}

extension Core {
    /// Runs a command from the palette, as its menu item would.
    func run(_ command: CommandId, tab: UInt32, window: UInt32) {
        let home = FileManager.default.homeDirectoryForCurrentUser.path
        switch command {
        case .back: update(.back(tab: tab))
        case .forward: update(.forward(tab: tab))
        case .up: update(.up(tab: tab))
        case .refresh: update(.refresh(tab: tab))
        case .find: find(tab: tab)
        case .goToFolder: update(.openGoTo(tab: tab))
        case .palette: update(.setPaletteText(text: ""))
        case .settings: NSApp.sendAction(Selector(("showSettingsWindow:")), to: nil, from: nil)
        case .toggleSidebar: NSApp.sendAction(#selector(NSSplitViewController.toggleSidebar(_:)), to: nil, from: nil)
        case .toggleInspector: update(.setInspector(shown: !view.inspectorShown))
        case .toggleHidden:
            if let shown = self.tab(tab) { update(.setShowHidden(tab: tab, show: !shown.view.showHidden)) }
        case .viewList: update(.setDisplay(tab: tab, display: .list))
        case .viewIcons: update(.setDisplay(tab: tab, display: .icons))
        case .newTab: newTab(of: window, path: self.tab(tab)?.path ?? home)
        case .newWindow: update(.newWindow(path: home))
        case .closeTab: NSApp.keyWindow?.performClose(nil)
        case .reopenClosedTab: reopenClosedTab(of: window)
        case .newFolder: update(.newFolder(tab: tab))
        case .newTextFile: update(.newTextFile(tab: tab))
        case .rename: update(.startRename(tab: tab))
        case .duplicate: update(.duplicate(tab: tab))
        case .moveToTrash: trashSelection(tab: tab)
        case .deleteImmediately: purgeSelection(tab: tab)
        case .undo: update(.undo)
        case .redo: update(.redo)
        case .cut: update(.cutSelection(tab: tab))
        case .copy: update(.copySelection(tab: tab))
        case .paste: update(.paste(tab: tab))
        case .copyAsPath: update(.copyPaths(tab: tab))
        case .selectAll: update(.selectAll(tab: tab))
        case .invertSelection: update(.invertSelection(tab: tab))
        case .quickLook: toggleQuickLook(tab: tab)
        case .showInFinder: update(.revealSelection(tab: tab))
        case .getInfo: update(.getInfo(tab: tab))
        case .openInTerminal: update(.openInTerminal(tab: tab))
        case .saveSearch: saveSearch(tab: tab)
        case .openTrash: update(.openTrash(tab: tab))
        case .emptyTrash: emptyTrash()
        case .addToFavorites:
            if let path = self.tab(tab)?.path, !path.isEmpty { update(.addFavorite(path: path)) }
        case .shortcutHelp: showingShortcuts = true
        }
    }

    /// Runs a palette item in the window's focused tab.
    func run(_ target: PaletteTarget, tab: UInt32, window: UInt32) {
        update(.closePalette)
        switch target {
        case let .command(command): run(command, tab: tab, window: window)
        case let .folder(path): update(.navigate(tab: tab, path: path))
        case let .savedSearch(id): update(.runSavedSearch(tab: tab, id: id))
        case let .pluginAction(id): update(.runPluginAction(tab: tab, action: id))
        }
    }
}

extension PluginShortcutView {
    /// The key egui names, as AppKit writes menu keys; nil for one a menu
    /// can't show.
    var menuKey: Character? {
        let named: [String: Int] = [
            "Up": NSUpArrowFunctionKey, "Down": NSDownArrowFunctionKey,
            "Left": NSLeftArrowFunctionKey, "Right": NSRightArrowFunctionKey,
            "Escape": 0x1B, "Tab": NSTabCharacter, "Backspace": NSBackspaceCharacter,
            "Delete": NSDeleteFunctionKey, "Enter": NSCarriageReturnCharacter,
            "Home": NSHomeFunctionKey, "End": NSEndFunctionKey, "PageUp": NSPageUpFunctionKey,
            "PageDown": NSPageDownFunctionKey, "Space": 0x20, "Clear": NSClearLineFunctionKey,
            "Insert": NSInsertFunctionKey, "Help": NSHelpFunctionKey,
        ]
        if let code = named[key] { return UnicodeScalar(code).map(Character.init) }
        let characters: [String: Character] = [
            "Colon": ":", "Comma": ",", "Minus": "-", "Period": ".", "Plus": "+", "Equals": "=",
            "Semicolon": ";", "Backslash": "\\", "Slash": "/", "Pipe": "|", "Questionmark": "?",
            "Exclamationmark": "!", "OpenBracket": "[", "CloseBracket": "]",
            "OpenCurlyBracket": "{", "CloseCurlyBracket": "}", "Backtick": "`", "Quote": "'",
        ]
        if let character = characters[key] { return character }
        if key.count == 1 { return key.lowercased().first }
        if key.hasPrefix("F"), let number = Int(key.dropFirst()), (1...35).contains(number) {
            return UnicodeScalar(NSF1FunctionKey + number - 1).map(Character.init)
        }
        return nil
    }
}

extension String {
    /// "rename" → "Rename", for menu titles.
    var capitalizedFirst: String { prefix(1).uppercased() + dropFirst() }
}

extension Duration {
    var seconds: Double {
        let (whole, fraction) = components
        return Double(whole) + Double(fraction) / 1e18
    }
}

