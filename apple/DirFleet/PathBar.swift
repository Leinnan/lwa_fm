import App
import SwiftUI

/// The tab's folder as crumbs from an anchor (Home, a volume, the root). The
/// anchor's menu lists the folders above it; each chevron lists the
/// subfolders of the crumb before it. Double-click the bar to type a path.
struct PathBar: View, Equatable {
    let core: Core
    let tab: TabViewModel

    /// Drawn again only when its folder or crumbs change: measuring which
    /// crumbs fit is costly, and a click or a thumbnail changes the tab.
    nonisolated static func == (lhs: PathBar, rhs: PathBar) -> Bool {
        lhs.core === rhs.core && lhs.tab.id == rhs.tab.id && lhs.tab.path == rhs.tab.path
            && lhs.tab.title == rhs.tab.title && lhs.tab.crumbs == rhs.tab.crumbs
            && lhs.tab.crumbsAbove == rhs.tab.crumbsAbove && lhs.tab.children == rhs.tab.children
    }

    var body: some View {
        // The width decides which crumbs show; measured with the font, not by
        // laying the crumbs out, which SwiftUI would redo on every layout.
        GeometryReader { geometry in
            crumbRow(width: geometry.size.width)
                .frame(maxHeight: .infinity)
        }
        .frame(height: 22)
        .background(RoundedRectangle(cornerRadius: 6).fill(Color(nsColor: .controlBackgroundColor)))
        .onTapGesture(count: 2) { core.update(.openGoTo(tab: tab.id)) }
        .help("Double-click to go to a folder by typing its path")
    }

    /// Every crumb when they fit; otherwise, as egui's, the middle ones
    /// fold into a "…" menu between the first and the last two. If even
    /// those don't fit, the row is cut on the left: the folder you are in
    /// stays visible.
    @ViewBuilder
    private func crumbRow(width: CGFloat) -> some View {
        let all = Array(tab.crumbs.enumerated())
        let full = Self.width(of: tab.crumbs, folded: false)
        if full <= width || tab.crumbs.count <= 3 {
            crumbs(all, folded: []).frame(width: width, alignment: full <= width ? .leading : .trailing).clipped()
        } else {
            crumbs(
                all.filter { $0.offset == 0 || $0.offset >= tab.crumbs.count - 2 },
                folded: Array(tab.crumbs[1 ..< tab.crumbs.count - 2])
            )
            .frame(width: width, alignment: Self.width(of: tab.crumbs, folded: true) <= width ? .leading : .trailing)
            .clipped()
        }
    }

    /// About how wide the crumbs are: each title in its font, plus its icon,
    /// chevron and padding; folded, the first and last two and a "…".
    private static func width(of crumbs: [CrumbView], folded: Bool) -> CGFloat {
        let shown = folded ? crumbs.enumerated().filter { $0.offset == 0 || $0.offset >= crumbs.count - 2 }.map(\.element) : crumbs
        let regular = [NSAttributedString.Key.font: NSFont.systemFont(ofSize: NSFont.systemFontSize)]
        let bold = [NSAttributedString.Key.font: NSFont.systemFont(ofSize: NSFont.systemFontSize, weight: .semibold)]
        var total: CGFloat = 12 + (folded ? 30 : 0)
        for (index, crumb) in shown.enumerated() {
            let last = index == shown.count - 1
            total += (crumb.title as NSString).size(withAttributes: last ? bold : regular).width
            total += (crumb.anchor == nil ? 0 : 20) + 22
        }
        return total
    }

    private func crumbs(_ shown: [(offset: Int, element: CrumbView)], folded: [CrumbView]) -> some View {
        HStack(spacing: 2) {
                if tab.crumbs.isEmpty {
                    Text(tab.title).foregroundStyle(.secondary)
                }
                ForEach(shown, id: \.offset) { index, crumb in
                    if index == tab.crumbs.count - 2, !folded.isEmpty {
                        Menu("…") {
                            ForEach(folded, id: \.path) { hidden in
                                Button(hidden.title) { go(hidden.path) }
                            }
                        }
                        .menuStyle(.borderlessButton)
                        .fixedSize()
                        .help("Folders in between")
                        Image(systemName: "chevron.right").font(.caption2).foregroundStyle(.secondary)
                    }
                    if index == 0 && !tab.crumbsAbove.isEmpty {
                        Menu {
                            ForEach(tab.crumbsAbove.reversed(), id: \.path) { above in
                                Button(above.title) { go(above.path) }
                            }
                        } label: {
                            crumbLabel(crumb, last: tab.crumbs.count == 1)
                        } primaryAction: {
                            go(crumb.path)
                        }
                        .menuStyle(.borderlessButton)
                        .fixedSize()
                    } else {
                        Button { go(crumb.path, newTab: NSEvent.modifierFlags.contains(.command)) } label: {
                            crumbLabel(crumb, last: index == tab.crumbs.count - 1)
                        }
                        .help("⌘-click to open in a new tab")
                        .buttonStyle(.borderless)
                        .contextMenu { FolderActions(core: core, path: crumb.path) }
                    }
                    Chevron(core: core, tab: tab, path: crumb.path)
                        .id(index)
                        .dropDestination(for: URL.self) { urls, _ in
                            core.dropItems(tab: tab.id, urls: urls, target: crumb.path)
                        }
                }
        }
        .padding(.horizontal, 6)
        .frame(minHeight: 22)
        .fixedSize(horizontal: true, vertical: false)
    }

    /// Goes to `path`, or (`newTab`, as egui's ⌘-click) opens it in a new
    /// tab of the window.
    private func go(_ path: String, newTab: Bool = false) {
        if newTab, let window = core.window(showing: tab.id) {
            core.newTab(of: window.id, path: path)
        } else if path != tab.path {
            core.update(.navigate(tab: tab.id, path: path))
        }
    }

    @ViewBuilder
    private func crumbLabel(_ crumb: CrumbView, last: Bool) -> some View {
        let text = Text(crumb.title).fontWeight(last ? .semibold : .regular)
        switch crumb.anchor {
        case .home: Label { text } icon: { Image(systemName: "house") }
        case .volume, .root: Label { text } icon: { Image(systemName: "internaldrive") }
        case nil: text
        }
    }
}

/// The subfolders of `path`, read when the chevron is clicked.
private struct Chevron: View {
    let core: Core
    let tab: TabViewModel
    let path: String
    @State private var open = false

    var body: some View {
        Button {
            core.update(.listChildren(tab: tab.id, path: path))
            open = true
        } label: {
            Image(systemName: "chevron.right").font(.caption2).foregroundStyle(.secondary)
        }
        .buttonStyle(.borderless)
        .popover(isPresented: $open, arrowEdge: .bottom) {
            if let children = tab.children, children.path == path {
                if children.names.isEmpty {
                    Text("No folders").foregroundStyle(.secondary).padding()
                } else {
                    List(children.names, id: \.self) { name in
                        Button(name) {
                            open = false
                            core.update(.navigate(tab: tab.id, path: (path as NSString).appendingPathComponent(name)))
                        }
                        .buttonStyle(.borderless)
                    }
                    .frame(width: 240, height: min(CGFloat(children.names.count) * 24 + 16, 360))
                }
            } else {
                ProgressView().padding()
            }
        }
    }
}

/// Go to Folder: type a path ('~' is Home, a bare name finds favorites and
/// recent folders); the matching folders are listed below.
struct GoToSheet: View {
    let core: Core
    let tab: UInt32
    let goto: GoToView
    @State private var chosen: String?
    @FocusState private var focused: Bool

    var body: some View {
        VStack(alignment: .leading, spacing: 8) {
            Text("Go to Folder").font(.headline)
            TextField("Path", text: Binding(
                get: { goto.text },
                set: { core.update(.goToTyped(tab: tab, text: $0)) }
            ))
            .textFieldStyle(.roundedBorder)
            .focused($focused)
            .onSubmit { core.update(.goToSubmit(tab: tab, path: chosen)) }
            .onKeyPress(.downArrow) { move(1) }
            .onKeyPress(.upArrow) { move(-1) }
            .onKeyPress(.tab) {
                guard let first = chosen ?? goto.matches.first?.path else { return .ignored }
                core.update(.goToTyped(tab: tab, text: first + "/"))
                chosen = nil
                return .handled
            }
            if let error = goto.error {
                Text(error).foregroundStyle(.red).font(.callout)
            }
            List(goto.matches, id: \.path, selection: $chosen) { found in
                Label(found.title, systemImage: "folder")
                    .help(found.path)
                    .onTapGesture(count: 2) { core.update(.goToSubmit(tab: tab, path: found.path)) }
            }
            .frame(minHeight: 160)
            HStack {
                Spacer()
                Button("Cancel") { core.update(.closeGoTo(tab: tab)) }
                    .keyboardShortcut(.cancelAction)
                Button("Go") { core.update(.goToSubmit(tab: tab, path: chosen)) }
                    .keyboardShortcut(.defaultAction)
            }
        }
        .padding()
        .frame(width: 440)
        .onAppear { focused = true }
        .onChange(of: goto.text) { chosen = nil }
    }

    private func move(_ step: Int) -> KeyPress.Result {
        let paths = goto.matches.map(\.path)
        guard !paths.isEmpty else { return .ignored }
        let index = chosen.flatMap { paths.firstIndex(of: $0) }
        let next = index.map { $0 + step } ?? (step > 0 ? 0 : -1)
        chosen = next >= 0 && next < paths.count ? paths[next] : nil
        return .handled
    }
}
