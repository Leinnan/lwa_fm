import App
import Quartz
import SwiftUI

/// Owns the core, so it can say the app is quitting before windows close.
final class AppDelegate: NSObject, NSApplicationDelegate {
    @MainActor let core = Core(stateFile: AppDelegate.stateFile)

    /// Where this build keeps its state: its own bundle id, so it never
    /// shares data with the egui build. `-stateFile <path>` replaces it, so
    /// UI tests never touch the real one.
    private static var stateFile: String {
        if let file = UserDefaults.standard.string(forKey: "stateFile") {
            return file
        }
        let support = FileManager.default.urls(for: .applicationSupportDirectory, in: .userDomainMask)[0]
        return support
            .appendingPathComponent(Bundle.main.bundleIdentifier ?? "io.github.leinnan.dirfleet.native")
            .appendingPathComponent("state.json")
            .path
    }

    override init() {
        // The core restores windows itself; AppKit restoring them too would double them.
        UserDefaults.standard.register(defaults: ["NSQuitAlwaysKeepsWindows": false])
        super.init()
    }

    private var returnInQuickLook: Any?

    /// Return in the Quick Look panel opens what it shows, as Enter in egui's.
    @MainActor
    func applicationDidFinishLaunching(_: Notification) {
        returnInQuickLook = NSEvent.addLocalMonitorForEvents(matching: .keyDown) { [core] event in
            let isReturn = event.keyCode == 36 || event.keyCode == 76
            guard isReturn, event.window is QLPreviewPanel, core.openPreviewed() else { return event }
            return nil
        }
    }

    @MainActor
    func applicationShouldTerminate(_: NSApplication) -> NSApplication.TerminateReply {
        core.reportGroups()
        core.update(.quitting)
        return .terminateNow
    }
}

@main
struct DirFleetApp: App {
    @NSApplicationDelegateAdaptor private var delegate: AppDelegate
    private var core: Core { delegate.core }

    var body: some Scene {
        // One SwiftUI window per core window, named by its id.
        WindowGroup(for: UInt32.self) { $window in
            WindowContent(core: core, window: $window)
                .frame(minWidth: WindowContent.minSize.width, minHeight: WindowContent.minSize.height)
                .preferredColorScheme(core.view.settings.colorScheme)
        }
        .commands {
            SidebarCommands()
            FileCommands(core: core)
            SelectionCommands(core: core)
            ViewCommands(core: core)
            GoCommands(core: core)
            PluginCommands(core: core)
            CommandGroup(replacing: .help) {
                Button("Keyboard Shortcuts") { core.showingShortcuts = true }
                    .keyboardShortcut("/")
            }
            CommandGroup(after: .windowArrangement) {
                ForEach(1...9, id: \.self) { number in
                    Button("Show Tab \(number)") { Core.showTab(number) }
                        .keyboardShortcut(KeyEquivalent(Character("\(number)")))
                }
            }
        }
        SwiftUI.Settings {
            SettingsView(core: core)
                .preferredColorScheme(core.view.settings.colorScheme)
        }
    }
}

private struct FileCommands: Commands {
    let core: Core
    @FocusedValue(\.activeTab) private var tab
    @FocusedValue(\.activeWindow) private var window

    var body: some Commands {
        // The focused tab without its rows, and how much of it is selected:
        // a click that keeps the selection's size leaves the menu alone.
        let shown = core.view.shown(tab)
        let selected = core.view.selected(tab)
        CommandGroup(replacing: .newItem) {
            Button("New Window") { core.update(.newWindow(path: home)) }
                .keyboardShortcut("n")
            Button("New Tab") {
                if let window { core.newTab(of: window, path: shown?.path ?? home) }
            }
            .keyboardShortcut("t")
            .disabled(window == nil)
            Button("Duplicate Tab") {
                if let window { core.newTab(of: window, path: shown?.path ?? home) }
            }
            .disabled(window == nil)
            Button("Close Other Tabs") { core.closeOtherTabs() }
                .disabled(window == nil)
            Button("Reopen Closed Tab") {
                if let window { core.reopenClosedTab(of: window) }
            }
            .keyboardShortcut("t", modifiers: [.command, .shift])
            .disabled(window == nil)
            Button("Save Search…") {
                if let tab { core.saveSearch(tab: tab) }
            }
            .keyboardShortcut("s")
            .disabled(shown?.search.map { $0.text.isEmpty && $0.chips.isEmpty } ?? true)
            Button("Update Saved Search") {
                if let tab { core.update(.updateSavedSearch(tab: tab)) }
            }
            .disabled(shown?.saved?.modified != true)
            Button("Revert to Saved Search") {
                if let tab { core.update(.revertSearch(tab: tab)) }
            }
            .disabled(shown?.saved?.modified != true)
            Divider()
            Button("Open Folder…") {
                if let tab { core.update(.openFolder(tab: tab)) }
            }
            .keyboardShortcut("o")
            .disabled(tab == nil)
            Divider()
            Button("Open") {
                if let tab { core.update(.openSelection(tab: tab)) }
            }
            .keyboardShortcut(.downArrow)
            .disabled(selected == .none)
            Button("Open in New Tab") {
                if let tab, let window, let path = shown?.path, !path.isEmpty {
                    core.newTab(of: window, path: core.selectedFolder(tab: tab) ?? path)
                }
            }
            .keyboardShortcut("o", modifiers: [.command, .control])
            .disabled(window == nil)
            Button("Open With…") {
                if let tab { core.update(.openWith(tab: tab)) }
            }
            .disabled(selected == .none)
            Button("Show Package Contents") {
                if let tab { core.update(.showPackageContents(tab: tab)) }
            }
            .disabled(selected != .one)
            Button("Show in Finder") {
                if let tab { core.update(.revealSelection(tab: tab)) }
            }
            .keyboardShortcut("r", modifiers: [.command, .option])
            .disabled(tab == nil)
            Button("Get Info") {
                if let tab { core.update(.getInfo(tab: tab)) }
            }
            .keyboardShortcut("i")
            .disabled(tab == nil)
            Button("Open in Terminal") {
                if let tab { core.update(.openInTerminal(tab: tab)) }
            }
            .disabled(shown?.path.isEmpty ?? true)
            Button("Quick Look") {
                if let tab { core.toggleQuickLook(tab: tab) }
            }
            .keyboardShortcut("y")
            .disabled(tab == nil)
            Divider()
            if let path = shown?.path, !path.isEmpty {
                let favorite = core.view.favorites.contains(path)
                Button(favorite ? "Remove from Favorites" : "Add to Favorites") {
                    core.update(favorite ? .removeFavorite(path: path) : .addFavorite(path: path))
                }
                .keyboardShortcut("t", modifiers: [.command, .control])
                Divider()
            }
            Button("New Folder") {
                if let tab { core.update(.newFolder(tab: tab)) }
            }
            .keyboardShortcut("n", modifiers: [.command, .shift])
            .disabled(shown?.path.isEmpty ?? true)
            Button("New Text Document") {
                if let tab { core.update(.newTextFile(tab: tab)) }
            }
            .disabled(shown?.path.isEmpty ?? true)
            Button(selected == .many ? "Rename Items…" : "Rename") {
                if let tab { core.update(.startRename(tab: tab)) }
            }
            .disabled(shown.map { $0.inTrash || selected == .none } ?? true)
            Button("Duplicate") {
                if let tab { core.update(.duplicate(tab: tab)) }
            }
            .keyboardShortcut("d")
            .disabled(shown.map { $0.inTrash || selected == .none } ?? true)
            if let shown {
                TransferMenu(core: core, tab: shown, selected: selected != .none, copy: true)
                TransferMenu(core: core, tab: shown, selected: selected != .none, copy: false)
            }
            Divider()
            Button("Move to \(core.view.trash.label)") {
                if let tab { core.trashSelection(tab: tab) }
            }
            .keyboardShortcut(.delete)
            .disabled(shown.map { $0.inTrash || selected == .none } ?? true)
            Button("Delete Immediately…") {
                if let tab { core.purgeSelection(tab: tab) }
            }
            .keyboardShortcut(.delete, modifiers: [.command, .option])
            .disabled(selected == .none)
            if core.view.trash.canRestore {
                Button("Put Back") {
                    if let tab { core.update(.restoreSelection(tab: tab)) }
                }
                .disabled(shown.map { !$0.inTrash || selected == .none } ?? true)
            }
            Button("Empty \(core.view.trash.label)…") { core.emptyTrash() }
                .keyboardShortcut(.delete, modifiers: [.command, .shift])
        }
    }
}

private struct SelectionCommands: Commands {
    let core: Core
    @FocusedValue(\.activeTab) private var tab
    @FocusedValue(\.activeWindow) private var window

    var body: some Commands {
        CommandGroup(replacing: .undoRedo) {
            Button(core.view.undo.map { "Undo \($0.capitalizedFirst)" } ?? "Undo") { core.undo(redo: false) }
                .keyboardShortcut("z")
            Button(core.view.redo.map { "Redo \($0.capitalizedFirst)" } ?? "Redo") { core.undo(redo: true) }
                .keyboardShortcut("z", modifiers: [.command, .shift])
        }
        CommandGroup(after: .pasteboard) {
            Button("Copy as Pathname") {
                if let tab { core.update(.copyPaths(tab: tab)) }
            }
            .keyboardShortcut("c", modifiers: [.command, .option])
            .disabled(tab == nil)
            Button("Commands…") {
                core.paletteWindow = window
                core.update(.setPaletteText(text: ""))
            }
            .keyboardShortcut("p", modifiers: [.command, .shift])
            .disabled(window == nil)
            Button("Find") {
                if let tab { core.find(tab: tab) }
            }
            .keyboardShortcut("f")
            .disabled(tab == nil)
            Divider()
            Button("Invert Selection") {
                if let tab { core.update(.invertSelection(tab: tab)) }
            }
            .keyboardShortcut("i", modifiers: [.command, .shift])
            .disabled(tab == nil)
            Button("Deselect All") {
                if let tab { core.update(.clearSelection(tab: tab)) }
            }
            .keyboardShortcut("a", modifiers: [.command, .option])
            .disabled(core.view.selected(tab) == .none)
        }
    }
}

private struct GoCommands: Commands {
    let core: Core
    @FocusedValue(\.activeTab) private var tab

    var body: some Commands {
        CommandMenu("Go") {
            let nav = core.view.shown(tab)?.nav
            Button("Back") { if let tab { core.update(.back(tab: tab)) } }
                .keyboardShortcut("[")
                .disabled(nav?.back != true)
            Button("Forward") { if let tab { core.update(.forward(tab: tab)) } }
                .keyboardShortcut("]")
                .disabled(nav?.forward != true)
            Button("Enclosing Folder") { if let tab { core.update(.up(tab: tab)) } }
                .keyboardShortcut(.upArrow)
                .disabled(nav?.up != true)
            Divider()
            Button("Go to Folder…") { if let tab { core.update(.openGoTo(tab: tab)) } }
                .keyboardShortcut("g", modifiers: [.command, .shift])
                .disabled(tab == nil)
            Button("Edit Path") { if let tab { core.update(.openGoTo(tab: tab)) } }
                .keyboardShortcut("l")
                .disabled(tab == nil)
            Button(core.view.trash.label) { if let tab { core.update(.openTrash(tab: tab)) } }
                .disabled(tab == nil)
        }
    }
}

/// Plugin actions with shortcuts, as egui runs them from its keymap: on the
/// focused tab's selection.
private struct PluginCommands: Commands {
    let core: Core
    @FocusedValue(\.activeTab) private var tab

    var body: some Commands {
        CommandMenu("Plugins") {
            ForEach(core.view.pluginShortcuts, id: \.action) { binding in
                let button = Button(binding.title) { run(binding) }.disabled(tab == nil)
                if let key = binding.menuKey {
                    button.keyboardShortcut(KeyEquivalent(key), modifiers: binding.modifiers)
                } else {
                    button
                }
            }
            if !core.view.pluginShortcuts.isEmpty { Divider() }
            Button("Reload Plugins") { core.update(.reloadPlugins) }
        }
    }

    /// Through the menu's action when the tab lists it, so it asks first
    /// when the plugin says to.
    private func run(_ binding: PluginShortcutView) {
        guard let tab else { return }
        let shown = core.tab(tab).map { $0.pluginActions + $0.folderPluginActions } ?? []
        if let action = shown.first(where: { $0.id == binding.action }) {
            core.runPluginAction(action, tab: tab)
        } else {
            core.update(.runPluginAction(tab: tab, action: binding.action))
        }
    }
}

extension PluginShortcutView {
    var modifiers: EventModifiers {
        var modifiers: EventModifiers = []
        if command { modifiers.insert(.command) }
        if alt { modifiers.insert(.option) }
        if shift { modifiers.insert(.shift) }
        return modifiers
    }
}

private struct ViewCommands: Commands {
    let core: Core
    @FocusedValue(\.activeTab) private var tab
    @FocusedValue(\.activeWindow) private var window

    var body: some Commands {
        CommandGroup(before: .toolbar) {
            if let tab = core.view.shown(tab) {
                Menu("Sort By") {
                    ViewOptions(core: core, tab: tab)
                }
                Button("Refresh") { core.update(.refresh(tab: tab.id)) }
                    .keyboardShortcut("r")
                Button(core.view.inspectorShown ? "Hide Inspector" : "Show Inspector") {
                    core.update(.setInspector(shown: !core.view.inspectorShown))
                }
                .keyboardShortcut("i", modifiers: [.command, .option])
                Divider()
                Button("Split Right") { core.update(.splitNew(tab: tab.id, side: .right)) }
                Button("Split Below") { core.update(.splitNew(tab: tab.id, side: .below)) }
                Button("Close Split") { core.update(.closeTab(tab: tab.id)) }
                    .disabled((window.flatMap { core.view.windowStores[$0] }?.shell.panes.count ?? 1) < 2)
                Divider()
            }
        }
    }
}
