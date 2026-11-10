import App
import SwiftUI

/// The command palette: type to find a command, folder or saved search;
/// arrows choose, Return runs.
struct CommandPalette: View {
    let core: Core
    let palette: PaletteModel
    let tab: UInt32
    let window: UInt32
    @State private var chosen = 0
    @FocusState private var focused: Bool

    var body: some View {
        VStack(alignment: .leading, spacing: 6) {
            TextField("Type a command, folder or saved search", text: Binding(
                get: { palette.text },
                set: { core.update(.setPaletteText(text: $0)) }
            ))
            .textFieldStyle(.roundedBorder)
            .focused($focused)
            .onSubmit(runChosen)
            .onKeyPress(.downArrow) { move(1) }
            .onKeyPress(.upArrow) { move(-1) }
            .onExitCommand { core.update(.closePalette) }
            ScrollViewReader { proxy in
                List(Array(palette.items.enumerated()), id: \.offset) { index, item in
                    HStack {
                        Image(systemName: icon(item.target))
                            .frame(width: 18)
                            .foregroundStyle(.secondary)
                        Text(item.title)
                        Spacer()
                        Text(item.detail).foregroundStyle(.secondary).font(.callout)
                    }
                    .padding(.vertical, 2)
                    .listRowBackground(index == chosen ? Color.accentColor.opacity(0.25) : Color.clear)
                    .contentShape(Rectangle())
                    .onTapGesture { core.run(item.target, tab: tab, window: window) }
                    .id(index)
                }
                .onChange(of: chosen) { proxy.scrollTo(chosen) }
            }
            .frame(height: 320)
        }
        .padding(12)
        .frame(width: 520)
        .onAppear { focused = true }
        .onChange(of: palette.text) { chosen = 0 }
    }

    private func move(_ step: Int) -> KeyPress.Result {
        guard !palette.items.isEmpty else { return .ignored }
        chosen = min(max(chosen + step, 0), palette.items.count - 1)
        return .handled
    }

    private func runChosen() {
        guard chosen < palette.items.count else { return }
        core.run(palette.items[chosen].target, tab: tab, window: window)
    }

    private func icon(_ target: PaletteTarget) -> String {
        switch target {
        case .command: "command"
        case .folder: "folder"
        case .savedSearch: "magnifyingglass"
        case .pluginAction: "puzzlepiece.extension"
        }
    }
}

/// Help ▸ Keyboard Shortcuts: every command and its key.
struct ShortcutsList: View {
    let core: Core

    var body: some View {
        VStack(alignment: .leading) {
            Text("Keyboard Shortcuts").font(.headline)
            List(core.view.commands.filter { !$0.shortcut.isEmpty }, id: \.label) { command in
                HStack {
                    Text(command.label)
                    Spacer()
                    Text(command.shortcut).monospaced().foregroundStyle(.secondary)
                }
            }
            .frame(height: 420)
            HStack {
                Spacer()
                Button("Done") { core.showingShortcuts = false }.keyboardShortcut(.defaultAction)
            }
        }
        .padding()
        .frame(width: 420)
    }
}
