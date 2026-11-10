import App
import AppKit
import QuickLook
import SwiftUI

/// One tab's folder: its header, its rows, its status bar and its sheets.
/// It reads the tab without rows and selection: those go from the tab's
/// store to the AppKit views in `FolderRows`, so a click, a scroll or a
/// thumbnail runs nothing here.
struct FolderView: View {
    let core: Core
    let store: TabStore

    var body: some View {
        let tab = store.chrome
        VStack(spacing: 0) {
            FolderHeader(core: core, tab: tab).equatable()
            Divider()
            if let error = tab.error, !tab.denied {
                Text(error).foregroundStyle(.red).padding()
            }
            if tab.denied {
                denied.frame(maxWidth: .infinity, maxHeight: .infinity)
            } else if tab.totalRows == 0 && tab.error != nil {
                Spacer()
            } else if tab.totalRows == 0 && !tab.loading {
                empty.frame(maxWidth: .infinity, maxHeight: .infinity)
            } else {
                FolderRows(core: core, store: store, config: FileViewConfig(
                    columns: core.view.columns,
                    optionalColumns: core.view.optionalColumns,
                    iconSize: core.view.settings.iconSize
                ))
                    // Escape with the search open: clear the text, then the
                    // filters, then close it, as egui's.
                    .onExitCommand {
                        if tab.search != nil { core.update(.searchEscape(tab: tab.id)) }
                    }
            }
            if !tab.pluginBar.isEmpty {
                PluginBar(core: core, tab: tab)
            }
            StatusBar(core: core, store: store)
        }
        .frame(maxWidth: .infinity, maxHeight: .infinity)
        .sheet(isPresented: Binding(
            get: { tab.batchRename != nil },
            set: { if !$0 && tab.batchRename != nil { core.update(.cancelBatchRename(tab: tab.id)) } }
        )) {
            if let batch = tab.batchRename {
                BatchRenameSheet(core: core, tab: tab.id, batch: batch)
            }
        }
        .sheet(isPresented: Binding(
            get: { core.view.pluginResults?.tab == tab.id },
            set: { if !$0 && core.view.pluginResults?.tab == tab.id { core.update(.hidePluginResults) } }
        )) {
            if let results = core.view.pluginResults {
                PluginResultsSheet(core: core, results: results)
            }
        }
        .sheet(isPresented: Binding(
            get: { core.view.pluginForm?.tab == tab.id },
            set: { if !$0 && core.view.pluginForm?.tab == tab.id { core.update(.cancelPluginForm) } }
        )) {
            if let form = core.view.pluginForm {
                PluginFormSheet(core: core, form: form)
            }
        }
        .sheet(isPresented: Binding(
            get: { core.view.thumbnailPicker?.tab == tab.id },
            set: { if !$0 && core.view.thumbnailPicker?.tab == tab.id { core.update(.closePicker) } }
        )) {
            if let picker = core.view.thumbnailPicker {
                ThumbnailPickerSheet(core: core, picker: picker)
            }
        }
        .sheet(isPresented: Binding(
            get: { tab.goto != nil },
            set: { if !$0 && tab.goto != nil { core.update(.closeGoTo(tab: tab.id)) } }
        )) {
            if let goto = tab.goto {
                GoToSheet(core: core, tab: tab.id, goto: goto)
            }
        }
        .quickLookPreview(previewURL, in: tab.id == core.previewTab ? core.previewItems : [])
    }

    private var tab: TabViewModel { store.chrome }

    /// Quick Look's item, only in the window that asked for it.
    private var previewURL: Binding<URL?> {
        Binding(
            get: { tab.id == core.previewTab ? core.previewURL : nil },
            set: { core.previewURL = $0 }
        )
    }

    // MARK: Empty

    /// As egui's: the folder can't be read, and where to allow it.
    private var denied: some View {
        ContentUnavailableView {
            Label("DirFleet doesn't have permission to open this folder", systemImage: "lock")
        } description: {
            Text(tab.error ?? "")
        } actions: {
            Button("Open Privacy Settings") { core.update(.openPrivacySettings) }
        }
    }

    @ViewBuilder private var empty: some View {
        if let search = tab.search, !search.text.isEmpty || !search.chips.isEmpty {
            // As egui's: nothing matched here, so offer to look wider.
            ContentUnavailableView {
                Label("No Results", systemImage: "magnifyingglass")
            } description: {
                if search.offerSubfolders {
                    Text("Nothing matched in this folder.")
                }
            } actions: {
                if search.offerSubfolders {
                    // The core's WIDER_DEPTH.
                    Button("Search Subfolders") { core.update(.setSearchDepth(tab: tab.id, depth: 4)) }
                }
                if search.offerFavorites {
                    Button("Search Favorites") { core.update(.setSearchFavorites(tab: tab.id, on: true)) }
                }
            }
        } else {
            folderEmpty
        }
    }

    private var folderEmpty: some View {
        ContentUnavailableView {
            if tab.inTrash {
                Label("The \(core.view.trash.label) is empty", systemImage: "trash")
            } else {
                Label("This folder is empty", systemImage: "folder")
            }
        } actions: {
            if tab.hiddenCount > 0 {
                Button(tab.hiddenCount == 1 ? "Show 1 hidden item" : "Show \(tab.hiddenCount) hidden items") {
                    core.update(.setShowHidden(tab: tab.id, show: true))
                }
            }
        }
    }

}

/// A tab's rows as a list or as icons, drawn by AppKit (`FolderController`).
/// SwiftUI gives it only what the rows are drawn with; the rows themselves
/// reach it from the tab's store.
private struct FolderRows: NSViewRepresentable {
    let core: Core
    let store: TabStore
    let config: FileViewConfig

    func makeCoordinator() -> FolderController {
        FolderController(core: core)
    }

    func makeNSView(context: Context) -> NSView {
        context.coordinator.view
    }

    func updateNSView(_: NSView, context: Context) {
        context.coordinator.configure(config)
        context.coordinator.attach(store)
    }

    static func dismantleNSView(_: NSView, coordinator: FolderController) {
        coordinator.detach()
    }
}

/// A video's or GIF's frames as an animated GIF the core made, playing on a
/// loop; SwiftUI's `Image` shows only the first frame.
struct AnimatedImage: NSViewRepresentable {
    let file: String

    func makeNSView(context _: Context) -> NSImageView {
        let view = NSImageView()
        view.animates = true
        view.imageScaling = .scaleProportionallyUpOrDown
        view.setContentCompressionResistancePriority(.defaultLow, for: .horizontal)
        view.setContentCompressionResistancePriority(.defaultLow, for: .vertical)
        return view
    }

    func updateNSView(_ view: NSImageView, context: Context) {
        guard context.coordinator.file != file else { return }
        context.coordinator.file = file
        let file = file
        // Read off the main thread; the frames decode as they play.
        Task {
            let data = await Task.detached(priority: .userInitiated) { try? Data(contentsOf: URL(fileURLWithPath: file)) }.value
            if context.coordinator.file == file, let data {
                view.image = NSImage(data: data)
            }
        }
    }

    func makeCoordinator() -> Shown { Shown() }

    /// The file the view shows, so it is read only when it changes.
    final class Shown {
        var file: String?
    }
}

/// A window's toolbar, acting on its focused tab.
struct WindowToolbar: ToolbarContent {
    let core: Core
    let tab: TabViewModel

    var body: some ToolbarContent {
        ToolbarItem {
            Picker("View", selection: Binding(
                get: { tab.view.display },
                set: { core.update(.setDisplay(tab: tab.id, display: $0)) }
            )) {
                Label("as Icons", systemImage: "square.grid.2x2").tag(DisplayType.icons)
                Label("as List", systemImage: "list.bullet").tag(DisplayType.list)
            }
            .pickerStyle(.segmented)
            .help("Show as icons or as a list")
        }
        if let search = tab.search {
            ToolbarItem {
                SearchOptions(core: core, tab: tab, search: search)
            }
        }
        ToolbarItem {
            Button {
                core.update(.setInspector(shown: !core.view.inspectorShown))
            } label: {
                Label("Inspector", systemImage: "sidebar.right")
            }
            .help(core.view.inspectorShown ? "Hide the inspector" : "Show the inspector")
        }
        if tab.inTrash {
            ToolbarItem {
                Button {
                    core.emptyTrash()
                } label: {
                    Label("Empty \(core.view.trash.label)…", systemImage: "trash.slash")
                }
                .help("Delete everything in the \(core.view.trash.label) for good")
            }
        }
        ToolbarItem {
            ViewOptionsMenu(core: core, tab: tab).equatable()
        }
        if !core.view.activity.isEmpty {
            ToolbarItem {
                ActivityButton(core: core)
            }
        }
        ToolbarItem {
            MoreMenu(core: core, tab: tab).equatable()
        }
    }
}

/// The toolbar's sort and hidden-items menu.
private struct ViewOptionsMenu: View, Equatable {
    let core: Core
    let tab: TabViewModel

    nonisolated static func == (lhs: ViewOptionsMenu, rhs: ViewOptionsMenu) -> Bool {
        lhs.core === rhs.core && lhs.tab == rhs.tab
    }

    var body: some View {
        Menu {
            ViewOptions(core: core, tab: tab)
        } label: {
            Label("View options", systemImage: "arrow.up.arrow.down")
        }
        .help("Sort and show hidden items")
    }
}

/// As egui's toolbar More menu: the view, panels, the folder's commands
/// and the app's.
private struct MoreMenu: View, Equatable {
    let core: Core
    let tab: TabViewModel

    nonisolated static func == (lhs: MoreMenu, rhs: MoreMenu) -> Bool {
        lhs.core === rhs.core && lhs.tab == rhs.tab
    }

    var body: some View {
        Menu {
            ViewOptions(core: core, tab: tab)
            Divider()
            Button(core.view.inspectorShown ? "Hide Inspector" : "Show Inspector") {
                core.update(.setInspector(shown: !core.view.inspectorShown))
            }
            Divider()
            Button("Refresh") { core.update(.refresh(tab: tab.id)) }
            Button("Go to Folder…") { core.update(.openGoTo(tab: tab.id)) }
            Button("Open in Terminal") { core.update(.openInTerminal(tab: tab.id)) }
                .disabled(tab.path.isEmpty)
            Button("New Folder") { core.update(.newFolder(tab: tab.id)) }
                .disabled(tab.path.isEmpty)
            Button("New Text Document") { core.update(.newTextFile(tab: tab.id)) }
                .disabled(tab.path.isEmpty)
            Divider()
            Button("Commands…") { core.update(.setPaletteText(text: "")) }
            Button("Keyboard Shortcuts") { core.showingShortcuts = true }
            SettingsLink { Text("Settings…") }
        } label: {
            Label("More", systemImage: "ellipsis.circle")
        }
        .help("More actions")
    }
}

/// A folder's header: navigation, the path bar, loading and progress.
/// Given the tab without its rows and selection, so a click or a thumbnail
/// doesn't draw it again.
private struct FolderHeader: View, Equatable {
    let core: Core
    let tab: TabViewModel

    nonisolated static func == (lhs: FolderHeader, rhs: FolderHeader) -> Bool {
        lhs.core === rhs.core && lhs.tab == rhs.tab
    }

    var body: some View {
    HStack {
        ControlGroup {
            Button { core.update(.back(tab: tab.id)) } label: { Image(systemName: "chevron.left") }
                .disabled(!tab.nav.back)
                .help("Back")
            Button { core.update(.forward(tab: tab.id)) } label: { Image(systemName: "chevron.right") }
                .disabled(!tab.nav.forward)
                .help("Forward")
        }
        .fixedSize()
        Button { core.update(.up(tab: tab.id)) } label: { Image(systemName: "arrow.up") }
            .disabled(!tab.nav.up)
            .help("Enclosing folder")
        PathBar(core: core, tab: tab).equatable()
        if tab.loading {
            ProgressView().controlSize(.small)
        }
        if !tab.progress.isEmpty {
            Text(tab.progress).font(.caption).foregroundStyle(.secondary).fixedSize()
        }
        if core.view.cutCount > 0 {
            Text("\(core.view.cutCount) cut · paste to move \(core.view.cutCount == 1 ? "it" : "them")")
                .font(.caption)
                .foregroundStyle(.secondary)
                .fixedSize()
        }
        if let search = tab.search, !search.text.isEmpty || !search.chips.isEmpty {
            Text(tab.totalRows == 1 ? "1 result" : "\(tab.totalRows) results")
                .foregroundStyle(.secondary)
                .monospacedDigit()
                .fixedSize()
            if search.searching {
                Button("Stop") { core.update(.stopSearch(tab: tab.id)) }
                    .help("Stop searching subfolders, keeping what was found")
            }
        }
        ForEach(core.view.operations, id: \.id) { operation in
            ProgressView(value: Double(operation.progress), total: 1000) {
                Text(operation.description).font(.caption).lineLimit(1)
            }
            .frame(width: 160)
            .help(operation.current)
            Button {
                core.update(.cancelOperation(id: operation.id))
            } label: {
                Image(systemName: "xmark.circle.fill")
            }
            .buttonStyle(.borderless)
            .help("Stop")
        }
        ForEach(core.view.pluginJobs, id: \.id) { job in
            ProgressView(value: Double(job.progress), total: 1000) {
                if job.results.isEmpty {
                    Text(job.waiting ? "\(job.title) (waiting)" : job.title).font(.caption).lineLimit(1)
                } else {
                    // A results dialog's job: its title brings the dialog back.
                    Button(job.title) { core.update(.showPluginResults(key: job.results)) }
                        .buttonStyle(.link)
                        .font(.caption)
                        .lineLimit(1)
                }
            }
            .frame(width: 160)
            .help(job.label)
            Button {
                core.update(.cancelPluginJob(id: job.id))
            } label: {
                Image(systemName: "xmark.circle.fill")
            }
            .buttonStyle(.borderless)
            .help(job.waiting ? "Don't run it" : "Cancel")
        }
    }
    .padding(8)
    }
}

/// The last finished file operations, as egui's Activity menu, with Undo.
private struct ActivityButton: View {
    let core: Core
    @State private var shown = false

    var body: some View {
        Button {
            shown.toggle()
        } label: {
            Label("Activity", systemImage: "clock.arrow.circlepath")
        }
        .help("Recent file operations")
        .popover(isPresented: $shown, arrowEdge: .bottom) {
            VStack(alignment: .leading, spacing: 6) {
                Text("Recent Activity").font(.headline)
                ForEach(core.view.activity, id: \.id) { item in
                    HStack {
                        switch item.state {
                        case .done: Image(systemName: "checkmark.circle.fill").foregroundStyle(.green)
                        case .failed: Image(systemName: "xmark.circle.fill").foregroundStyle(.red)
                        case .stopped: Image(systemName: "minus.circle").foregroundStyle(.secondary)
                        }
                        Text(item.text).lineLimit(1)
                        Spacer()
                        Text(item.when).foregroundStyle(.secondary).font(.caption)
                    }
                    .help(item.errors.joined(separator: "\n"))
                }
                if let undo = core.view.undo {
                    Divider()
                    Button("Undo \(undo)") {
                        core.update(.undo)
                        shown = false
                    }
                }
            }
            .padding()
            .frame(width: 340)
        }
    }
}

/// Move To or Copy To: other tabs' folders, favorites and drives, then a
/// folder picker, as egui's submenu.
struct TransferMenu: View {
    let core: Core
    let tab: TabViewModel
    /// Whether the tab has a selection to send.
    let selected: Bool
    let copy: Bool

    var body: some View {
        EntryMenu(entries: [FileMenus.transfer(core: core, tab: tab, selected: selected, copy: copy)])
    }
}

/// Sort, reverse and hidden items for a tab: the toolbar menu and the View menu.
struct ViewOptions: View {
    let core: Core
    let tab: TabViewModel

    var body: some View {
        EntryMenu(entries: FileMenus.viewOptions(core: core, tab: tab))
    }
}

/// What every folder offers wherever it is shown: the folder itself, a
/// crumb, a sidebar row.
struct FolderActions: View {
    let core: Core
    let path: String

    var body: some View {
        EntryMenu(entries: FileMenus.folderActions(core: core, path: path))
    }
}

/// Menu entries as SwiftUI's menu items: the menus the file views show as
/// an `NSMenu`, in the menu bar, the toolbar, the sidebar and the path bar.
struct EntryMenu: View {
    let entries: [MenuEntry]

    var body: some View {
        ForEach(Array(entries.enumerated()), id: \.offset) { _, entry in
            EntryItem(entry: entry)
        }
    }
}

private struct EntryItem: View {
    let entry: MenuEntry

    var body: some View {
        switch entry.kind {
        case .separator:
            Divider()
        case .header:
            Text(entry.title)
        case let .submenu(entries):
            Menu(entry.title) { EntryMenu(entries: entries) }
                .disabled(!entry.enabled)
        case let .action(run):
            if let key = entry.key {
                item(run).keyboardShortcut(KeyEquivalent(key.0), modifiers: Self.modifiers(key.1))
            } else {
                item(run)
            }
        }
    }

    @ViewBuilder
    private func item(_ run: @escaping @MainActor () -> Void) -> some View {
        if let checked = entry.checked {
            Toggle(entry.title, isOn: Binding(get: { checked }, set: { _ in run() }))
                .disabled(!entry.enabled)
                .help(entry.help)
        } else {
            Button(entry.title) { run() }
                .disabled(!entry.enabled)
                .help(entry.help)
        }
    }

    private static func modifiers(_ flags: NSEvent.ModifierFlags) -> EventModifiers {
        var modifiers: EventModifiers = []
        if flags.contains(.command) { modifiers.insert(.command) }
        if flags.contains(.shift) { modifiers.insert(.shift) }
        if flags.contains(.option) { modifiers.insert(.option) }
        if flags.contains(.control) { modifiers.insert(.control) }
        return modifiers
    }
}
