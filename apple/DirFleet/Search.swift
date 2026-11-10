import App
import SwiftUI

/// One of a search's chips, shown as a token in the search field.
struct SearchToken: Identifiable, Hashable {
    let id: Int
    let token: String
    let error: String?
}

/// The window's toolbar search field, acting on its focused tab: the search's
/// chips are its tokens, This Folder / Subfolders its scopes. Every edit goes
/// to the core, which filters as you type.
struct TabSearch: ViewModifier {
    let core: Core
    let tab: TabViewModel
    @State private var presented = false

    func body(content: Content) -> some View {
        content
            .searchable(
                text: text,
                tokens: tokens,
                isPresented: $presented,
                placement: .toolbar,
                prompt: "Search"
            ) { token in
                if token.error != nil {
                    Label(token.token, systemImage: "exclamationmark.triangle")
                } else {
                    Text(token.token)
                }
            }
            .searchScopes(scope) {
                Text("This Folder").tag(Scope.folder)
                Text("Subfolders").tag(Scope.subfolders)
                if hasFavorites {
                    Text("Favorites").tag(Scope.favorites)
                }
            }
            .onSubmit(of: .search) { core.update(.commitSearchText(tab: tab.id)) }
            .onChange(of: presented) { _, shown in
                if !shown && tab.search != nil { core.update(.closeSearch(tab: tab.id)) }
            }
            .onChange(of: tab.id, initial: true) { presented = tab.search != nil }
            .onChange(of: core.findRequest) { _, request in
                if request?.tab == tab.id { presented = true }
            }
    }

    private var text: Binding<String> {
        Binding(
            get: { tab.search?.text ?? "" },
            set: { text in
                if text != (tab.search?.text ?? "") || tab.search == nil {
                    core.update(.setSearchText(tab: tab.id, text: text))
                }
            }
        )
    }

    private var tokens: Binding<[SearchToken]> {
        Binding(
            get: {
                (tab.search?.chips ?? []).enumerated().map { index, chip in
                    SearchToken(id: index, token: chip.token, error: chip.error)
                }
            },
            set: { kept in
                // The field removes tokens one at a time.
                let left = Set(kept.map(\.id))
                let chips = tab.search?.chips.indices ?? 0..<0
                if let gone = chips.first(where: { !left.contains($0) }) {
                    core.update(.removeSearchChip(tab: tab.id, index: UInt32(gone)))
                }
            }
        )
    }

    private enum Scope: Hashable {
        case folder, subfolders, favorites
    }

    private var hasFavorites: Bool { core.view.hasFavorites }

    private var scope: Binding<Scope> {
        Binding(
            get: {
                if tab.search?.scope == .favorites { return .favorites }
                return (tab.search?.depth ?? 1) > 1 ? .subfolders : .folder
            },
            set: { scope in
                core.update(.setSearchFavorites(tab: tab.id, on: scope == .favorites))
                switch scope {
                case .folder: core.update(.setSearchDepth(tab: tab.id, depth: 1))
                case .subfolders: core.update(.setSearchDepth(tab: tab.id, depth: 4))
                case .favorites: break
                }
            }
        )
    }
}

/// Match case, how words combine, how deep to look and the preset filters,
/// for the tab's open search.
struct SearchOptions: View {
    let core: Core
    let tab: TabViewModel
    let search: SearchView

    var body: some View {
        HStack(spacing: 4) {
            options
            SearchHelp(core: core)
        }
    }

    private var options: some View {
        Menu {
            Toggle("Match Case", isOn: Binding(
                get: { search.caseSensitive },
                set: { core.update(.setSearchCase(tab: tab.id, on: $0)) }
            ))
            Picker("Names Match", selection: Binding(
                get: { search.any },
                set: { core.update(.setSearchAny(tab: tab.id, any: $0)) }
            )) {
                Text("All Words").tag(false)
                Text("Any Word").tag(true)
            }
            Picker("Look In", selection: Binding(
                get: { search.depth },
                set: { core.update(.setSearchDepth(tab: tab.id, depth: $0)) }
            )) {
                Text("This Folder").tag(UInt32(1))
                ForEach(UInt32(2)...7, id: \.self) { depth in
                    Text("Subfolders, \(depth) Levels Deep").tag(depth)
                }
            }
            Divider()
            ForEach(core.view.searchFilters, id: \.label) { group in
                Menu(group.label) {
                    ForEach(group.choices, id: \.token) { choice in
                        Button {
                            core.update(.toggleSearchFilter(tab: tab.id, token: choice.token))
                        } label: {
                            if search.filtersOn.contains(choice.token) {
                                Label(choice.label, systemImage: "checkmark")
                            } else {
                                Text(choice.label)
                            }
                        }
                    }
                }
            }
        } label: {
            Label("Search options", systemImage: "line.3.horizontal.decrease.circle")
        }
        .help("Match case, subfolders and filters")
    }
}

/// The search syntax, as egui's help popup.
struct SearchHelp: View {
    let core: Core
    @State private var shown = false

    var body: some View {
        Button {
            shown.toggle()
        } label: {
            Label("Search help", systemImage: "questionmark.circle")
        }
        .help("How to write a search")
        .popover(isPresented: $shown, arrowEdge: .bottom) {
            VStack(alignment: .leading, spacing: 8) {
                Text("Search Syntax").font(.headline)
                Grid(alignment: .leading, horizontalSpacing: 16, verticalSpacing: 4) {
                    ForEach(core.view.searchHelp, id: \.example) { row in
                        GridRow {
                            Text(row.example).monospaced()
                            Text(row.meaning).foregroundStyle(.secondary)
                        }
                    }
                }
                Text("Return turns text into filters; Delete removes the last one.")
                    .font(.caption)
                    .foregroundStyle(.secondary)
            }
            .padding()
        }
    }
}
