import App
import SwiftUI

/// Settings ▸ Plugins, as egui's page: each plugin with its switch, what it
/// may do and why it stopped, and the folders that couldn't be loaded.
struct PluginsSettings: View {
    let core: Core

    var body: some View {
        Form {
            ForEach(core.view.plugins.plugins, id: \.id) { plugin in
                PluginRow(core: core, plugin: plugin)
            }
            if !core.view.plugins.broken.isEmpty {
                Section("Couldn't be loaded") {
                    ForEach(core.view.plugins.broken, id: \.folder) { broken in
                        VStack(alignment: .leading, spacing: 2) {
                            Text(broken.folder).lineLimit(1).truncationMode(.middle)
                            Text(broken.message).font(.caption).foregroundStyle(.red)
                        }
                    }
                }
            }
            Section {
                Button("Reload Plugins") { core.update(.reloadPlugins) }
            }
        }
        .formStyle(.grouped)
    }
}

private struct PluginRow: View {
    let core: Core
    let plugin: PluginView

    var body: some View {
        Section {
            Toggle(isOn: Binding(
                get: { plugin.enabled },
                set: { core.update(.setPluginEnabled(id: plugin.id, enabled: $0)) }
            )) {
                VStack(alignment: .leading, spacing: 2) {
                    Text(plugin.name).font(.headline)
                    Text("\(plugin.version) · \(plugin.author)\(plugin.bundled ? " · Built in" : "")")
                        .font(.caption)
                        .foregroundStyle(.secondary)
                }
            }
            if !plugin.description.isEmpty {
                Text(plugin.description).foregroundStyle(.secondary)
            }
            LabeledContent("Status", value: plugin.status)
            if !plugin.permissions.isEmpty {
                LabeledContent("May") {
                    Text(plugin.permissions.joined(separator: ", ")).multilineTextAlignment(.trailing)
                }
            }
            if !plugin.ffmpeg.isEmpty {
                LabeledContent("FFmpeg", value: plugin.ffmpeg)
                    .help(plugin.ffmpegPath)
            }
            if plugin.needsApproval {
                HStack {
                    Text("It runs once you allow what it asks for.").foregroundStyle(.secondary)
                    Spacer()
                    Button("Allow") { core.update(.approvePlugin(id: plugin.id)) }
                }
            }
            if !plugin.shortcuts.isEmpty {
                DisclosureGroup("Shortcuts") {
                    ForEach(plugin.shortcuts, id: \.action) { setting in
                        PluginShortcutField(setting: setting) {
                            core.update(.setPluginShortcut(plugin: plugin.id, action: setting.action, text: $0))
                        }
                    }
                }
            }
            if !plugin.errors.isEmpty {
                DisclosureGroup("Errors (\(plugin.errors.count))") {
                    ForEach(Array(plugin.errors.enumerated()), id: \.offset) { _, error in
                        Text(error).font(.caption.monospaced()).textSelection(.enabled)
                    }
                }
            }
        }
    }
}

/// One action's shortcut, written as egui writes them ("Ctrl+Alt+M", Ctrl
/// being ⌘ here); sent when the user presses Return or leaves the field.
private struct PluginShortcutField: View {
    let setting: PluginShortcutSetting
    let set: (String) -> Void
    @State private var text = ""
    @FocusState private var focused: Bool

    var body: some View {
        VStack(alignment: .leading, spacing: 2) {
            TextField(setting.title, text: $text, prompt: Text("None"))
                .focused($focused)
                .onSubmit { set(text) }
                .onChange(of: focused) { _, now in
                    if !now, text != setting.text { set(text) }
                }
            if !setting.note.isEmpty {
                Text(setting.note).font(.caption).foregroundStyle(.orange)
            }
        }
        .onAppear { text = setting.text }
        .onChange(of: setting.text) { _, now in text = now }
    }
}
