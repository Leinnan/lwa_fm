import App
import SwiftUI

/// A window's sidebar, Finder style: the core's sections (Favorites, Recent,
/// Locations, Drives), each collapsible. A row shows its folder in the
/// window's focused tab.
struct SidebarView: View, Equatable {
    let core: Core
    let window: UInt32
    /// The window's focused tab and the folder it shows.
    let tab: UInt32
    let path: String
    let sections: [SidebarSectionView]

    /// Drawn again only when its sections or the folder shown change, not
    /// for every change to the tab.
    nonisolated static func == (lhs: SidebarView, rhs: SidebarView) -> Bool {
        lhs.core === rhs.core && lhs.window == rhs.window && lhs.sections == rhs.sections
            && lhs.tab == rhs.tab && lhs.path == rhs.path
    }
    /// The chosen row. Kept here and followed with `onChange`: a List on
    /// macOS doesn't drive a computed selection binding reliably.
    @State private var selected: String?

    var body: some View {
        List(selection: $selected) {
            ForEach(sections, id: \.title) { section in
                Section(isExpanded: expanded(section)) {
                    // Ids unique across sections: a folder can be in several.
                    ForEach(section.items.map { (Self.id(of: $0, in: section.section), $0) }, id: \.0) { id, item in
                        SidebarRow(core: core, window: window, tab: tab, item: item)
                            .tag(Optional(id))
                    }
                    .onMove(perform: section.section == .favorites ? moveFavorites
                        : section.section == .savedSearches ? moveSaved : nil)
                } header: {
                    Text(section.title)
                        .contextMenu { SectionMenu(core: core, shown: sections.map(\.section), section: section) }
                }
            }
        }
        .listStyle(.sidebar)
        .navigationSplitViewColumnWidth(min: 160, ideal: 200, max: 320)
        .onChange(of: activeID, initial: true) { _, id in selected = id }
        .onChange(of: selected) { _, id in
            if let id, id != activeID { choose(id) }
        }
    }

    /// Rows are told apart by section, since a folder can be in several.
    private static func id(of item: SidebarItem, in section: SidebarSection) -> String {
        "\(section):\(item.trash ? ":trash" : item.path)"
    }

    /// The row showing what the focused tab shows.
    private var activeID: String? {
        for section in sections {
            if let item = section.items.first(where: \.active) {
                return Self.id(of: item, in: section.section)
            }
        }
        return nil
    }

    /// Goes to the row `id`.
    private func choose(_ id: String) {
        for section in sections {
            if let item = section.items.first(where: { Self.id(of: $0, in: section.section) == id }) {
                go(to: item)
                return
            }
        }
    }

    private func go(to item: SidebarItem) {
        if let id = item.savedSearch {
            core.update(.runSavedSearch(tab: tab, id: id))
        } else if item.trash {
            core.update(.openTrash(tab: tab))
        } else if item.path != path {
            core.update(.navigate(tab: tab, path: item.path))
        }
    }

    private func expanded(_ section: SidebarSectionView) -> Binding<Bool> {
        Binding(
            get: { !section.collapsed },
            set: { core.update(.setSectionCollapsed(section: section.section, collapsed: !$0)) }
        )
    }

    private func moveSaved(from: IndexSet, to: Int) {
        guard let source = from.first else { return }
        let destination = to > source ? to - 1 : to
        if destination != source {
            core.update(.moveSavedSearch(from: UInt32(source), to: UInt32(destination)))
        }
    }

    private func moveFavorites(from: IndexSet, to: Int) {
        guard let source = from.first else { return }
        // SwiftUI counts the destination before the row is taken out.
        let destination = to > source ? to - 1 : to
        if destination != source {
            core.update(.moveFavorite(from: UInt32(source), to: UInt32(destination)))
        }
    }
}

private struct SidebarRow: View {
    let core: Core
    let window: UInt32
    let tab: UInt32
    let item: SidebarItem

    var body: some View {
        HStack {
            VStack(alignment: .leading, spacing: 2) {
                Label(item.title, systemImage: symbol(item.icon))
                    .lineLimit(1)
                if let used = item.usedPermille {
                    ProgressView(value: Double(used), total: 1000)
                        .controlSize(.mini)
                        .padding(.leading, 24)
                }
            }
            if let eject = item.eject {
                Spacer()
                Button {
                    core.update(.ejectDrive(mount: item.path))
                } label: {
                    Image(systemName: "eject")
                }
                .buttonStyle(.borderless)
                .help("\(eject) \(item.title)")
            }
        }
        .help(item.detail)
        .dropDestination(for: URL.self) { urls, _ in
            if item.trash {
                core.update(.dropOnTrash(tab: tab, paths: urls.filter(\.isFileURL).map(\.path)))
                return true
            }
            guard item.savedSearch == nil, !item.path.isEmpty else { return false }
            return core.dropItems(tab: tab, urls: urls, target: item.path)
        }
        .contextMenu {
            if let id = item.savedSearch {
                SavedSearchMenu(core: core, window: window, tab: tab, id: id)
            } else if !item.trash {
                FolderActions(core: core, path: item.path)
            } else {
                Button("Empty \(core.view.trash.label)…") { core.emptyTrash() }
            }
            if let eject = item.eject {
                Divider()
                Button(eject) { core.update(.ejectDrive(mount: item.path)) }
            }
        }
    }
}

/// The SF Symbol of a sidebar icon.
func symbol(_ icon: SidebarIcon) -> String {
    switch icon {
    case .folder: "folder"
    case .home: "house"
    case .documents: "doc"
    case .desktop: "menubar.dock.rectangle"
    case .downloads: "arrow.down.circle"
    case .pictures: "photo"
    case .music: "music.note"
    case .recent: "clock"
    case .trash: "trash"
    case .savedSearch: "magnifyingglass"
    case let .drive(kind):
        switch kind {
        case .fixed: "internaldrive"
        case .removable: "externaldrive"
        case .network: "network"
        case .optical: "opticaldisc"
        case .virtual: "memorychip"
        }
    }
}

/// A saved search's menu: run, edit, duplicate, copy its query, delete.
/// A section header's menu, as egui's: move it among the shown sections,
/// collapse or expand it, or all of them.
private struct SectionMenu: View {
    let core: Core
    /// The sections the sidebar shows, in order.
    let shown: [SidebarSection]
    let section: SidebarSectionView

    var body: some View {
        let index = shown.firstIndex(of: section.section) ?? 0
        Button(section.collapsed ? "Expand" : "Collapse") {
            core.update(.setSectionCollapsed(section: section.section, collapsed: !section.collapsed))
        }
        Divider()
        Button("Move Up") {
            core.update(.moveSidebarSection(section: section.section, to: shown[index - 1]))
        }
        .disabled(index == 0)
        Button("Move Down") {
            core.update(.moveSidebarSection(section: section.section, to: shown[index + 1]))
        }
        .disabled(index + 1 >= shown.count)
        Divider()
        Button("Collapse All") {
            for other in shown { core.update(.setSectionCollapsed(section: other, collapsed: true)) }
        }
        Button("Expand All") {
            for other in shown { core.update(.setSectionCollapsed(section: other, collapsed: false)) }
        }
    }
}

private struct SavedSearchMenu: View {
    let core: Core
    let window: UInt32
    let tab: UInt32
    let id: UInt64

    var body: some View {
        Button("Run") { core.update(.runSavedSearch(tab: tab, id: id)) }
        Button("Run in New Tab") { core.runSavedSearchInNewTab(of: window, from: tab, id: id) }
        Button("Edit…") { core.editingSavedSearch = id }
        Button("Duplicate") { core.update(.duplicateSavedSearch(id: id)) }
        Button("Copy Query") {
            if let saved = core.view.savedSearches.first(where: { $0.id == id }) {
                NSPasteboard.general.clearContents()
                NSPasteboard.general.setString(saved.text, forType: .string)
            }
        }
        Divider()
        Button("Delete") { core.update(.deleteSavedSearch(id: id)) }
    }
}

/// Edits a saved search: its name, query and where it runs.
struct SavedSearchEditor: View {
    let core: Core
    let saved: SavedSearchView
    @State private var name = ""
    @State private var text = ""
    @State private var caseSensitive = false
    @State private var any = false
    @State private var depth: UInt32 = 1
    @State private var favorites = false

    var body: some View {
        Form {
            TextField("Name", text: $name)
            TextField("Search for", text: $text)
            Toggle("Match case", isOn: $caseSensitive)
            Picker("Names match", selection: $any) {
                Text("All words").tag(false)
                Text("Any word").tag(true)
            }
            Picker("Look in", selection: $depth) {
                Text("The folder only").tag(UInt32(1))
                ForEach(UInt32(2)...7, id: \.self) { depth in
                    Text("Subfolders, \(depth) levels deep").tag(depth)
                }
            }
            Toggle("Search the favorites, not the open folder", isOn: $favorites)
            HStack {
                Spacer()
                Button("Cancel") { core.editingSavedSearch = nil }
                    .keyboardShortcut(.cancelAction)
                Button("Save") {
                    core.update(.editSavedSearch(
                        id: saved.id, name: name, text: text, caseSensitive: caseSensitive,
                        depth: depth, any: any, favorites: favorites
                    ))
                    core.editingSavedSearch = nil
                }
                .keyboardShortcut(.defaultAction)
                .disabled(name.trimmingCharacters(in: .whitespaces).isEmpty)
            }
        }
        .formStyle(.grouped)
        .frame(width: 420)
        .onAppear {
            name = saved.name
            text = saved.text
            caseSensitive = saved.caseSensitive
            any = saved.any
            depth = saved.depth
            favorites = saved.favorites
        }
    }
}
