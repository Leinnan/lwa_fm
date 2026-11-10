import App
import Foundation
import Observation

/// How much of a tab is selected, for commands that ask only that.
enum SelectionKind {
    case none, one, many

    init(count: UInt32) {
        self = count == 0 ? .none : count == 1 ? .one : .many
    }
}

/// The numbers under a folder, for its status bar.
struct TabCounts: Equatable {
    var totalRows: UInt32
    var selectedCount: UInt32
    var driveStatus: DriveStatusView?
}

/// What the inspector shows of a tab.
struct InspectorSlice: Equatable {
    var tab: UInt32
    var inTrash: Bool
    var inspector: InspectorView?
    var pluginSections: [PluginSectionView]
}

/// Draws a tab's rows without SwiftUI: the file views.
@MainActor
protocol TabRowsObserver: AnyObject {
    func apply(_ tab: TabViewModel)
}

/// One shown tab with one observed property per part, so a click, a scroll
/// or a thumbnail draws the folder's rows again and nothing around them.
@MainActor
@Observable
final class TabStore {
    let id: UInt32
    /// The tab without what changes with every click, scroll and thumbnail:
    /// for the header, the toolbar, the search field, sheets and menus.
    private(set) var chrome: TabViewModel
    private(set) var counts: TabCounts
    private(set) var selected: SelectionKind
    private(set) var inspector: InspectorSlice
    /// The whole tab, rows and selection included: for the folder's rows.
    private(set) var content: TabViewModel
    /// The file views drawing the rows, told of every change at once.
    @ObservationIgnored weak var rows: TabRowsObserver?

    init(_ tab: TabViewModel) {
        id = tab.id
        chrome = tab.chrome
        counts = tab.counts
        selected = SelectionKind(count: tab.selectedCount)
        inspector = tab.inspectorSlice
        content = tab
    }

    func update(_ tab: TabViewModel) {
        guard tab != content else { return }
        content = tab
        rows?.apply(tab)
        let chrome = tab.chrome
        if chrome != self.chrome { self.chrome = chrome }
        let counts = tab.counts
        if counts != self.counts { self.counts = counts }
        let selected = SelectionKind(count: tab.selectedCount)
        if selected != self.selected { self.selected = selected }
        let inspector = tab.inspectorSlice
        if inspector != self.inspector { self.inspector = inspector }
    }
}

/// What a window is made of, without its tabs' contents.
struct WindowShell: Equatable {
    var id: UInt32
    /// The tab each pane shows, in order.
    var panes: [UInt32]
    var focused: UInt32
    /// Whether two panes sit one above the other.
    var vertical: Bool
}

/// One window with one observed property per part.
@MainActor
@Observable
final class WindowStore {
    let id: UInt32
    private(set) var shell: WindowShell
    private(set) var sidebar: [SidebarSectionView]

    init(_ window: WindowView) {
        id = window.id
        shell = window.shell
        sidebar = window.sidebar
    }

    func update(_ window: WindowView) {
        let shell = window.shell
        if shell != self.shell { self.shell = shell }
        if window.sidebar != sidebar { sidebar = window.sidebar }
    }
}

extension WindowView {
    var shell: WindowShell {
        WindowShell(
            id: id,
            panes: panes.map(\.shown.id),
            focused: active.id,
            vertical: layout.splits.last?.vertical ?? false
        )
    }

    /// The tabs the window shows: each pane's, the focused one among them.
    var shownTabs: [TabViewModel] {
        let shown = panes.map(\.shown)
        return shown.contains { $0.id == active.id } ? shown : shown + [active]
    }
}

extension TabViewModel {
    /// The tab without what changes with every click, scroll and thumbnail
    /// (rows, selection, cursor, rename, the inspector, the drive's free
    /// space): for views around the folder that don't show those, so they
    /// compare equal and stay put.
    var chrome: TabViewModel {
        var chrome = self
        chrome.rows = []
        chrome.firstRow = 0
        chrome.selection = []
        chrome.selectedCount = 0
        chrome.cursor = nil
        chrome.renaming = nil
        chrome.scrollTo = nil
        chrome.scrollCount = 0
        chrome.inspector = nil
        chrome.pluginSections = []
        chrome.pluginActions = []
        chrome.driveStatus = nil
        return chrome
    }

    var counts: TabCounts {
        TabCounts(totalRows: totalRows, selectedCount: selectedCount, driveStatus: driveStatus)
    }

    var inspectorSlice: InspectorSlice {
        InspectorSlice(tab: id, inTrash: inTrash, inspector: inspector, pluginSections: pluginSections)
    }
}
