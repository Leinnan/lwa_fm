import App
import AppKit

/// One item of a menu, as data. The file views show a list of these as an
/// `NSMenu` built when the menu opens; SwiftUI's menus draw the same lists
/// (`EntryMenu`), so each menu is written once.
@MainActor
struct MenuEntry {
    enum Kind {
        case action(@MainActor () -> Void)
        case submenu([MenuEntry])
        case separator
        /// A title over the items after it.
        case header
    }

    var title = ""
    var kind: Kind
    var enabled = true
    /// Whether a checkable item is on; nil for one that isn't checkable.
    var checked: Bool?
    var help = ""
    /// Its key in the menu bar.
    var key: (Character, NSEvent.ModifierFlags)?

    static let separator = MenuEntry(kind: .separator)

    static func header(_ title: String) -> MenuEntry {
        MenuEntry(title: title, kind: .header)
    }

    static func item(
        _ title: String, enabled: Bool = true, checked: Bool? = nil, help: String = "",
        key: (Character, NSEvent.ModifierFlags)? = nil, _ run: @escaping @MainActor () -> Void
    ) -> MenuEntry {
        MenuEntry(title: title, kind: .action(run), enabled: enabled, checked: checked, help: help, key: key)
    }

    static func submenu(_ title: String, enabled: Bool = true, _ entries: [MenuEntry]) -> MenuEntry {
        MenuEntry(title: title, kind: .submenu(entries), enabled: enabled)
    }
}

/// The menus of a folder's rows and of its empty space.
@MainActor
enum FileMenus {
    /// The menu of the tab's selected rows: open, rename, move, copy, show.
    static func row(core: Core, tab: TabViewModel) -> [MenuEntry] {
        let id = tab.id
        let count = tab.selectedCount
        var entries: [MenuEntry] = [
            .item("Open") { core.update(.openSelection(tab: id)) },
            .item("Open With…") { core.update(.openWith(tab: id)) },
            .item("Quick Look") { core.toggleQuickLook(tab: id) },
            .separator,
            .item(count > 1 ? "Rename \(count) Items…" : "Rename", enabled: count > 0 && !tab.inTrash) {
                core.update(.startRename(tab: id))
            },
            .item("Duplicate") { core.update(.duplicate(tab: id)) },
        ]
        if count == 1, let video = tab.rows.first(where: \.selected), video.thumbnailChoice != .none {
            entries.append(.item("Select Thumbnail…") { core.update(.selectThumbnail(tab: id, row: video.index)) })
            if video.thumbnailChoice == .chosen {
                entries.append(.item("Reset Thumbnail") { core.update(.resetThumbnail(tab: id, row: video.index)) })
            }
        }
        entries += [
            .item("Move to \(core.view.trash.label)") { core.trashSelection(tab: id) },
            transfer(core: core, tab: tab, selected: count > 0, copy: false),
            transfer(core: core, tab: tab, selected: count > 0, copy: true),
            .separator,
            .item("Copy", enabled: !tab.inTrash) { core.update(.copySelection(tab: id)) },
            .item("Cut", enabled: !tab.inTrash) { core.update(.cutSelection(tab: id)) },
            .item("Copy as Pathname") { core.update(.copyPaths(tab: id)) },
            .separator,
            .item("Show in Finder") { core.update(.revealSelection(tab: id)) },
            .item("Get Info") { core.update(.getInfo(tab: id)) },
            .item("Remove from Open History") { core.update(.forgetOpened(tab: id)) },
        ]
        if !tab.pluginActions.isEmpty {
            entries.append(.separator)
            entries += tab.pluginActions.map { action in
                .item(action.title, help: action.plugin) { core.runPluginAction(action, tab: id) }
            }
        }
        return entries
    }

    /// The menu of Trash rows: put back where possible, delete for good,
    /// and show where it is.
    static func trashRow(core: Core, tab: TabViewModel) -> [MenuEntry] {
        let id = tab.id
        var entries: [MenuEntry] = []
        if core.view.trash.canRestore {
            entries.append(.item("Put Back") { core.update(.restoreSelection(tab: id)) })
        }
        entries += [
            .item("Delete Immediately…") { core.purgeSelection(tab: id) },
            .separator,
            .item("Show in Finder") { core.update(.revealSelection(tab: id)) },
            .item("Copy as Pathname") { core.update(.copyPaths(tab: id)) },
        ]
        return entries
    }

    /// The menu of the folder's empty space: new items, paste, view,
    /// refresh, undo.
    static func emptyArea(core: Core, tab: TabViewModel) -> [MenuEntry] {
        let id = tab.id
        var entries: [MenuEntry] = []
        if !tab.inTrash && !tab.path.isEmpty {
            entries += [
                .item("New Folder") { core.update(.newFolder(tab: id)) },
                .item("New Text Document") { core.update(.newTextFile(tab: id)) },
                .item("Paste") { core.update(.paste(tab: id)) },
                .separator,
            ]
        }
        entries += [
            .submenu("View", viewOptions(core: core, tab: tab)),
            .item("Refresh") { core.update(.refresh(tab: id)) },
        ]
        if let undo = core.view.undo {
            entries.append(.item("Undo \(undo.capitalizedFirst)") { core.update(.undo) })
        }
        if !tab.folderPluginActions.isEmpty {
            entries.append(.separator)
            entries += tab.folderPluginActions.map { action in
                .item(action.title, help: action.plugin) { core.runPluginAction(action, tab: id) }
            }
        }
        if !tab.path.isEmpty {
            entries.append(.separator)
            entries += folderActions(core: core, path: tab.path)
        }
        return entries
    }

    /// Move To or Copy To: other tabs' folders, favorites and drives, then a
    /// folder picker, as egui's submenu. `selected` says the tab has a
    /// selection to send.
    static func transfer(core: Core, tab: TabViewModel, selected: Bool, copy: Bool) -> MenuEntry {
        let id = tab.id
        var entries: [MenuEntry] = tab.transferTargets.map { folder in
            .item(folder.title, help: "\(folder.detail): \(folder.path)") {
                core.update(copy ? .copyTo(tab: id, folder: folder.path) : .moveTo(tab: id, folder: folder.path))
            }
        }
        if !entries.isEmpty {
            entries.append(.separator)
        }
        entries.append(.item("Choose Folder…") { core.sendSelection(tab: id, copy: copy) })
        return .submenu(copy ? "Copy To" : "Move To", enabled: !tab.inTrash && selected, entries)
    }

    /// Sort, reverse and hidden items for a tab: the toolbar menu, the View
    /// menu and the folder's own.
    static func viewOptions(core: Core, tab: TabViewModel) -> [MenuEntry] {
        let id = tab.id
        var entries: [MenuEntry] = [.header("Sort by")]
        entries += core.view.sorts.map { choice in
            .item(choice.label, checked: choice.sort == tab.view.sort) {
                if choice.sort != tab.view.sort { core.update(.sortBy(tab: id, sort: choice.sort)) }
            }
        }
        entries += [
            .separator,
            .item("Reverse order", enabled: tab.view.sort != .random, checked: tab.view.invert) {
                core.update(.setReverse(tab: id, reverse: !tab.view.invert))
            },
            .separator,
            .item("Show hidden items", checked: tab.view.showHidden, key: (".", [.command, .shift])) {
                core.update(.setShowHidden(tab: id, show: !tab.view.showHidden))
            },
            .separator,
            .item("as Icons", key: ("1", .command)) { core.update(.setDisplay(tab: id, display: .icons)) },
            .item("as List", key: ("2", .command)) { core.update(.setDisplay(tab: id, display: .list)) },
        ]
        return entries
    }

    /// What every folder offers wherever it is shown: the folder itself, a
    /// crumb, a sidebar row.
    static func folderActions(core: Core, path: String) -> [MenuEntry] {
        let favorite = core.view.favorites.contains(path)
        // The window showing the folder, or the first: found when an item is
        // chosen, so the menu doesn't follow every change to the windows.
        let window = { core.view.snapshot.first { $0.active.path == path } ?? core.view.snapshot.first }
        return [
            .item("Open in New Tab") {
                if let window = window() { core.newTab(of: window.id, path: path) }
            },
            .item("Paste Into Folder") {
                if let window = window() { core.update(.pasteInto(tab: window.active.id, folder: path)) }
            },
            .item(favorite ? "Remove from Favorites" : "Add to Favorites") {
                core.update(favorite ? .removeFavorite(path: path) : .addFavorite(path: path))
            },
            .item("Open in Finder") { core.update(.openInFileManager(path: path)) },
            .item("Show in Finder") { core.update(.revealPath(path: path)) },
            .item("Copy as Pathname") { core.update(.copyPath(path: path)) },
        ]
    }
}

/// Runs a menu item's closure; the item keeps it alive.
@MainActor
private final class MenuAction: NSObject {
    let run: @MainActor () -> Void

    init(_ run: @escaping @MainActor () -> Void) {
        self.run = run
    }

    @objc func chosen(_: Any?) {
        run()
    }
}

extension NSMenu {
    /// A menu of `entries`, for a file view's right-click.
    @MainActor
    convenience init(entries: [MenuEntry]) {
        self.init()
        autoenablesItems = false
        for entry in entries {
            addItem(Self.item(entry))
        }
    }

    @MainActor
    private static func item(_ entry: MenuEntry) -> NSMenuItem {
        switch entry.kind {
        case .separator:
            return .separator()
        case .header:
            return .sectionHeader(title: entry.title)
        case let .action(run):
            let action = MenuAction(run)
            let item = NSMenuItem(title: entry.title, action: #selector(MenuAction.chosen(_:)), keyEquivalent: "")
            item.target = action
            item.representedObject = action
            item.isEnabled = entry.enabled
            item.state = entry.checked == true ? .on : .off
            item.toolTip = entry.help.isEmpty ? nil : entry.help
            return item
        case let .submenu(entries):
            let item = NSMenuItem(title: entry.title, action: nil, keyEquivalent: "")
            item.submenu = NSMenu(entries: entries)
            item.isEnabled = entry.enabled
            return item
        }
    }
}
