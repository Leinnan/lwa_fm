import App
import AppKit
import SwiftUI

/// The tab in the focused window, for menu commands.
private struct ActiveTabKey: FocusedValueKey {
    typealias Value = UInt32
}

/// The focused core window, for menu commands.
private struct ActiveWindowKey: FocusedValueKey {
    typealias Value = UInt32
}

extension FocusedValues {
    var activeTab: UInt32? {
        get { self[ActiveTabKey.self] }
        set { self[ActiveTabKey.self] = newValue }
    }

    var activeWindow: UInt32? {
        get { self[ActiveWindowKey.self] }
        set { self[ActiveWindowKey.self] = newValue }
    }
}

var home: String {
    FileManager.default.homeDirectoryForCurrentUser.path
}

/// A folder to open instead of the saved windows: `-folder <path>` on the
/// command line, as UI tests pass it.
var launchFolder: String? {
    UserDefaults.standard.string(forKey: "folder")
}

/// One window: adopts a core window if it has none, reports when it closes.
struct WindowContent: View {
    /// The smallest a window can be.
    static let minSize = NSSize(width: 640, height: 420)

    let core: Core
    @Binding var window: UInt32?
    @Environment(\.openWindow) private var openWindow

    var body: some View {
        Group {
            if let window, let store = core.view.windowStores[window] {
                WindowBody(core: core, store: store)
            } else {
                ProgressView()
            }
        }
        .onAppear {
            if core.openWindow == nil {
                core.openWindow = { id in openWindow(value: id) }
            }
            // Opened by SwiftUI itself, as at launch: the core doesn't know it yet.
            if core.window(window) == nil {
                if core.started {
                    core.adopt(path: home) { id in window = id }
                } else {
                    core.start(open: launchFolder, home: home) { id in window = id }
                }
            }
        }
        .background(WindowObserver(
            shown: { nsWindow in if let window { core.register(window: window, nsWindow: nsWindow) } },
            closed: { if let window { core.update(.windowClosed(window: window)) } }
        ))
        .onChange(of: window) { _, id in
            // Adopted after the NSWindow was seen: register it now.
            if let id, let nsWindow = NSApp.keyWindow { core.register(window: id, nsWindow: nsWindow) }
        }
    }
}

/// A window's sidebar, panes, toolbar and sheets. It reads the window's
/// shape and its focused tab without rows, so it is drawn again when a pane
/// or the folder changes, not for a click or a thumbnail.
private struct WindowBody: View {
    let core: Core
    let store: WindowStore

    var body: some View {
        let shell = store.shell
        if let focused = core.view.tabs[shell.focused] {
            let tab = focused.chrome
            NavigationSplitView {
                SidebarView(core: core, window: shell.id, tab: tab.id, path: tab.path, sections: store.sidebar)
                    .equatable()
            } detail: {
                VStack(spacing: 0) {
                    if core.view.access.prompt != .hidden {
                        DiskAccessBanner(core: core)
                        Divider()
                    }
                    panes(shell)
                }
                    .inspector(isPresented: Binding(
                        get: { core.view.inspectorShown },
                        set: { if $0 != core.view.inspectorShown { core.update(.setInspector(shown: $0)) } }
                    )) {
                        InspectorPane(core: core, store: focused)
                    }
            }
                .modifier(TabSearch(core: core, tab: tab))
                .toolbar { WindowToolbar(core: core, tab: tab) }
                .navigationTitle(tab.title)
                .focusedSceneValue(\FocusedValues.activeTab, tab.id)
                .focusedSceneValue(\FocusedValues.activeWindow, shell.id)
                .modifier(WindowSheets(core: core, window: shell.id, tab: tab.id))
        } else {
            ProgressView()
        }
    }

    /// One pane, or the first two side by side: the Mac shows no deeper splits.
    @ViewBuilder
    private func panes(_ shell: WindowShell) -> some View {
        if shell.panes.count >= 2 {
            if shell.vertical {
                VSplitView {
                    pane(shell.panes[0], focused: shell.focused)
                    pane(shell.panes[1], focused: shell.focused)
                }
            } else {
                HSplitView {
                    pane(shell.panes[0], focused: shell.focused)
                    pane(shell.panes[1], focused: shell.focused)
                }
            }
        } else if let store = core.view.tabs[shell.focused] {
            FolderView(core: core, store: store)
        }
    }

    @ViewBuilder
    private func pane(_ tab: UInt32, focused: UInt32) -> some View {
        if let store = core.view.tabs[tab] {
            FolderView(core: core, store: store)
                .overlay(alignment: .top) {
                    if tab == focused {
                        Rectangle().fill(Color.accentColor).frame(height: 2)
                    }
                }
                .simultaneousGesture(TapGesture().onEnded {
                    if tab != focused { core.update(.activateTab(tab: tab)) }
                })
                .frame(minWidth: 240, minHeight: 160)
        }
    }
}

/// Calls `shown` with the hosting `NSWindow` once it exists, and `closed`
/// when it closes. `onDisappear` is not enough: it also fires when a window
/// tab goes to the background.
private struct WindowObserver: NSViewRepresentable {
    let shown: (NSWindow) -> Void
    let closed: () -> Void

    func makeNSView(context: Context) -> NSView {
        let view = NSView()
        DispatchQueue.main.async {
            guard let window = view.window else { return }
            Self.sizeByHand(window, hosting: view)
            shown(window)
            context.coordinator.token = NotificationCenter.default.addObserver(
                forName: NSWindow.willCloseNotification, object: window, queue: .main
            ) { _ in closed() }
        }
        return view
    }

    func updateNSView(_: NSView, context _: Context) {}

    func makeCoordinator() -> Coordinator { Coordinator() }

    /// Stops the hosting views around `view` from measuring the window's
    /// content for its size limits, and gives the window a fixed minimum
    /// instead. They measured it again after every change: a tenth of the
    /// main thread while scrolling.
    private static func sizeByHand(_ window: NSWindow, hosting view: NSView) {
        var next = view.superview
        while let host = next {
            (host as? SizedByHand)?.sizeByHand()
            next = host.superview
        }
        window.contentMinSize = WindowContent.minSize
    }

    final class Coordinator {
        var token: NSObjectProtocol?
        deinit {
            if let token { NotificationCenter.default.removeObserver(token) }
        }
    }
}


/// A hosting view that can stop sizing its window from its content.
private protocol SizedByHand: AnyObject {
    func sizeByHand()
}

extension NSHostingView: SizedByHand {
    func sizeByHand() {
        sizingOptions = []
    }
}

/// The core's toasts, newest at the bottom; click one to dismiss it.
private struct ToastStack: View {
    let core: Core

    var body: some View {
        VStack(alignment: .trailing, spacing: 6) {
            ForEach(core.view.toasts, id: \.id) { toast in
                Label(toast.text, systemImage: symbol(toast.kind))
                    .labelStyle(.titleAndIcon)
                    .foregroundStyle(color(toast.kind), .primary)
                    .padding(.horizontal, 12)
                    .padding(.vertical, 8)
                    .background(.regularMaterial, in: RoundedRectangle(cornerRadius: 8))
                    .shadow(radius: 4, y: 2)
                    .onTapGesture { core.update(.dismissToast(id: toast.id)) }
                    .transition(.move(edge: .trailing).combined(with: .opacity))
            }
        }
        .padding(16)
        .animation(.easeOut(duration: 0.2), value: core.view.toasts.map(\.id))
    }

    private func symbol(_ kind: ToastKind) -> String {
        switch kind {
        case .info: "info.circle.fill"
        case .success: "checkmark.circle.fill"
        case .warning: "exclamationmark.triangle.fill"
        case .error: "xmark.octagon.fill"
        }
    }

    private func color(_ kind: ToastKind) -> Color {
        switch kind {
        case .info: .blue
        case .success: .green
        case .warning: .orange
        case .error: .red
        }
    }
}

/// The mouse's back and forward buttons go back and forward in the window's
/// focused tab.
private struct MouseButtons: NSViewRepresentable {
    let core: Core
    let tab: UInt32

    func makeNSView(context: Context) -> NSView {
        let view = NSView()
        context.coordinator.monitor = NSEvent.addLocalMonitorForEvents(matching: .otherMouseDown) { event in
            guard event.window == view.window else { return event }
            switch event.buttonNumber {
            case 3: context.coordinator.step(back: true)
            case 4: context.coordinator.step(back: false)
            default: return event
            }
            return nil
        }
        return view
    }

    func updateNSView(_: NSView, context: Context) {
        context.coordinator.core = core
        context.coordinator.tab = tab
    }

    func makeCoordinator() -> Coordinator { Coordinator(core: core, tab: tab) }

    static func dismantleNSView(_: NSView, coordinator: Coordinator) {
        if let monitor = coordinator.monitor { NSEvent.removeMonitor(monitor) }
    }

    @MainActor
    final class Coordinator {
        var core: Core
        var tab: UInt32
        var monitor: Any?

        init(core: Core, tab: UInt32) {
            self.core = core
            self.tab = tab
        }

        func step(back: Bool) {
            core.update(back ? .back(tab: tab) : .forward(tab: tab))
        }
    }
}

/// As egui's: ejecting a drive that file operations still use asks first.
private struct EjectQuestion: ViewModifier {
    let core: Core
    let window: UInt32

    func body(content: Content) -> some View {
        let question = core.view.ejectConfirm
        content.confirmationDialog(
            question?.title ?? "",
            isPresented: Binding(
                get: { question != nil && core.view.firstWindow == window },
                set: { if !$0 && core.view.ejectConfirm != nil { core.update(.cancelEject) } }
            )
        ) {
            Button(question?.button ?? "Eject", role: .destructive) { core.update(.confirmEject) }
            Button("Cancel", role: .cancel) { core.update(.cancelEject) }
        } message: {
            Text(question?.message ?? "")
        }
    }
}

/// A window's dialogs, sheets, toasts and mouse buttons. One window shows
/// each app-wide dialog, not every window.
private struct WindowSheets: ViewModifier {
    let core: Core
    let window: UInt32
    /// The window's focused tab.
    let tab: UInt32

    func body(content: Content) -> some View {
        content
            .confirmationDialog(
            conflictTitle,
            isPresented: Binding(
                get: { core.view.conflict != nil && core.view.firstWindow == window },
                set: { if !$0 && core.view.conflict != nil { core.update(.resolveConflict(choice: .cancel)) } }
            )
        ) {
            Button("Replace") { core.update(.resolveConflict(choice: .replace)) }
            Button("Skip") { core.update(.resolveConflict(choice: .skip)) }
            Button("Keep Both") { core.update(.resolveConflict(choice: .keepBoth)) }
            Button("Cancel", role: .cancel) { core.update(.resolveConflict(choice: .cancel)) }
        } message: {
            Text("Replacing overwrites files and merges folders.")
        }
        .modifier(EjectQuestion(core: core, window: window))
        .overlay(alignment: .bottomTrailing) { ToastStack(core: core) }
        .background(MouseButtons(core: core, tab: tab))
        .sheet(isPresented: Binding(
            get: { core.view.palette != nil && (core.paletteWindow ?? core.view.firstWindow) == window },
            set: { if !$0 && core.view.palette != nil { core.update(.closePalette) } }
        )) {
            if let palette = core.view.palette {
                CommandPalette(core: core, palette: palette, tab: tab, window: window)
            }
        }
        .sheet(isPresented: Binding(
            get: { core.showingShortcuts && core.view.firstWindow == window },
            set: { core.showingShortcuts = $0 }
        )) {
            ShortcutsList(core: core)
        }
        .sheet(isPresented: Binding(
            get: { core.editingSavedSearch != nil && core.view.firstWindow == window },
            set: { if !$0 { core.editingSavedSearch = nil } }
        )) {
            if let saved = core.view.savedSearches.first(where: { $0.id == core.editingSavedSearch }) {
                SavedSearchEditor(core: core, saved: saved)
            }
        }
    }

    private var conflictTitle: String {
        guard let conflict = core.view.conflict else { return "" }
        let folder = (conflict.folder as NSString).lastPathComponent
        return conflict.count == 1
            ? "\"\(conflict.name)\" already exists in \"\(folder)\""
            : "\(conflict.count) items already exist in \"\(folder)\""
    }
}
