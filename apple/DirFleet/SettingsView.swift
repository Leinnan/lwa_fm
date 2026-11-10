import App
import SwiftUI

/// DirFleet ▸ Settings…: every edit goes to the core, which keeps it.
struct SettingsView: View {
    let core: Core

    var body: some View {
        TabView {
            GeneralSettings(core: core)
                .tabItem { Label("General", systemImage: "gearshape") }
            AppearanceSettings(core: core)
                .tabItem { Label("Appearance", systemImage: "paintpalette") }
            FolderSettings(core: core)
                .tabItem { Label("Folders", systemImage: "folder") }
            KeyboardSettings(core: core)
                .tabItem { Label("Keyboard", systemImage: "keyboard") }
            PluginsSettings(core: core)
                .tabItem { Label("Plugins", systemImage: "puzzlepiece.extension") }
        }
        .frame(width: 460)
        .padding()
    }
}

/// A binding to one setting: edits send the whole settings back.
@MainActor
func setting<T>(_ core: Core, _ key: WritableKeyPath<AppSettings, T>) -> Binding<T> {
    Binding(
        get: { core.view.settings[keyPath: key] },
        set: { value in
            var settings = core.view.settings
            settings[keyPath: key] = value
            core.update(.changeSettings(settings: settings))
        }
    )
}

private struct GeneralSettings: View {
    let core: Core

    var body: some View {
        Form {
            LabeledContent("Terminal") {
                HStack {
                    TextField("Terminal", text: setting(core, \.terminal)).labelsHidden()
                    Button("Choose…") {
                        Task {
                            if let app = await OpenPanelPicker().pickApp(title: "Terminal app") {
                                setting(core, \.terminal).wrappedValue =
                                    ((app as NSString).lastPathComponent as NSString).deletingPathExtension
                            }
                        }
                    }
                }
            }
            if !core.view.access.terminalFound {
                Text("No app with this name was found: check it, or choose one.")
                    .font(.caption)
                    .foregroundStyle(.orange)
            }
            Toggle("Ask before moving items to the \(core.view.trash.label)", isOn: setting(core, \.confirmTrash))
            Toggle("Ask whether to move or copy dropped items", isOn: setting(core, \.confirmDrop))
            LabeledContent("Open history") {
                HStack {
                    Toggle("Remember opened files", isOn: setting(core, \.trackOpenHistory))
                    Button("Clear…") { core.clearOpenHistory() }
                }
            }
            Section("Privacy") {
                LabeledContent("Full Disk Access") {
                    HStack {
                        Text(core.view.access.diskAccessGranted ? "On" : "Off")
                            .foregroundStyle(.secondary)
                        Button("Open System Settings") { core.update(.openPrivacySettings) }
                    }
                }
                .help(core.view.access.diskAccessGranted
                    ? "Every folder opens without asking"
                    : "macOS asks before DirFleet opens Desktop, Documents, Downloads and drives")
                Toggle("Suggest Full Disk Access at launch", isOn: setting(core, \.askFullDiskAccess))
            }
            Section("About") {
                LabeledContent("DirFleet", value: core.view.access.version)
                if let website = URL(string: core.view.access.website) {
                    Link("Project website", destination: website)
                }
            }
            Section {
                Button("Restore Defaults") { core.update(.restoreDefaultSettings) }
            }
        }
        .formStyle(.grouped)
    }
}

/// Every command with a shortcut, as egui's Keyboard page.
private struct KeyboardSettings: View {
    let core: Core

    var body: some View {
        List {
            ForEach(core.view.commands.filter { !$0.shortcut.isEmpty }, id: \.label) { command in
                ShortcutRow(label: command.label, shortcut: command.shortcut)
            }
            if !core.view.pluginShortcuts.isEmpty {
                Section("Plugins") {
                    ForEach(core.view.pluginShortcuts, id: \.action) { binding in
                        ShortcutRow(label: binding.title, shortcut: binding.text)
                    }
                }
            }
        }
        .frame(height: 360)
    }
}

private struct ShortcutRow: View {
    let label: String
    let shortcut: String

    var body: some View {
        HStack {
            Text(label)
            Spacer()
            Text(shortcut).monospaced().foregroundStyle(.secondary)
        }
    }
}

private struct AppearanceSettings: View {
    let core: Core

    var body: some View {
        Form {
            Picker("Theme", selection: setting(core, \.appearance)) {
                Text("System").tag(Appearance.system)
                Text("Light").tag(Appearance.light)
                Text("Dark").tag(Appearance.dark)
            }
            Picker("Icon size", selection: setting(core, \.iconSize)) {
                ForEach(core.view.iconSizes, id: \.label) { choice in
                    Text(choice.label).tag(choice.size)
                }
            }
            Toggle("Play the selected video or GIF", isOn: setting(core, \.animateSelectedPreviews))
                .help("Grid tiles play when the pointer rests on them; this plays the selected one too")
            LabeledContent("Chosen video thumbnails") {
                Button("Reset All…") { core.resetAllThumbnails() }
            }
            Section("Sidebar") {
                Toggle("Recent folders", isOn: setting(core, \.showRecent))
                Toggle(core.view.trash.label, isOn: setting(core, \.showTrash))
                Toggle("How full each drive is", isOn: setting(core, \.showDriveCapacity))
                Toggle("Say when drives connect or disconnect", isOn: setting(core, \.notifyDriveChanges))
            }
        }
        .formStyle(.grouped)
    }
}

private struct FolderSettings: View {
    let core: Core

    var body: some View {
        Form {
            Section("Folders without a view of their own") {
                Picker("Show as", selection: defaults(\.display)) {
                    Text("Icons").tag(DisplayType.icons)
                    Text("List").tag(DisplayType.list)
                }
                Picker("Sort by", selection: defaults(\.sort)) {
                    ForEach(core.view.sorts, id: \.label) { choice in
                        Text(choice.label).tag(choice.sort)
                    }
                }
                Toggle("Reverse order", isOn: defaults(\.invert))
                Toggle("Show hidden items", isOn: defaults(\.showHidden))
            }
        }
        .formStyle(.grouped)
    }

    private func defaults<T>(_ key: WritableKeyPath<FolderLook, T>) -> Binding<T> {
        Binding(
            get: { core.view.folderDefaults[keyPath: key] },
            set: { value in
                var view = core.view.folderDefaults
                view[keyPath: key] = value
                core.update(.setFolderDefaults(view: view))
            }
        )
    }
}

extension AppSettings {
    /// The color scheme windows use; `nil` follows the system.
    var colorScheme: ColorScheme? {
        switch appearance {
        case .system: nil
        case .light: .light
        case .dark: .dark
        }
    }
}
