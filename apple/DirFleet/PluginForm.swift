import App
import SwiftUI

/// A plugin action's form, as egui's dialog: the fields the core lists, in
/// order, each edit sent to the core, which checks the values and asks the
/// plugin before the action runs.
struct PluginFormSheet: View {
    let core: Core
    let form: PluginFormView

    var body: some View {
        VStack(alignment: .leading, spacing: 12) {
            Text(form.title).font(.headline)
            if form.loading {
                ProgressView().frame(maxWidth: .infinity, minHeight: 80)
            } else if !form.error.isEmpty {
                Text("The form couldn't be made: \(form.error)").foregroundStyle(.red)
            } else {
                ScrollView {
                    Form {
                        ForEach(form.fields, id: \.id) { field in
                            PluginFieldRow(field: field) {
                                core.update(.setPluginFormField(id: field.id, text: $0))
                            }
                                .padding(.leading, CGFloat(field.depth) * 16)
                                .disabled(!field.enabled)
                        }
                    }
                    .formStyle(.columns)
                }
                .frame(maxHeight: 420)
            }
            if !form.message.isEmpty {
                Text(form.message).foregroundStyle(.red)
            }
            HStack {
                if form.busy { ProgressView().controlSize(.small) }
                Spacer()
                if form.blocked {
                    Button("Close") { core.update(.cancelPluginForm) }
                        .keyboardShortcut(.cancelAction)
                } else {
                    Button("Cancel", role: .cancel) { core.update(.cancelPluginForm) }
                        .keyboardShortcut(.cancelAction)
                    Button(form.submit) { core.update(.submitPluginForm) }
                        .keyboardShortcut(.defaultAction)
                        .tint(form.danger ? .red : nil)
                        .disabled(form.busy || form.loading || !form.error.isEmpty)
                }
            }
        }
        .padding()
        .frame(width: 480)
    }
}

/// One field of a plugin's form or results options; edits go to `set`.
struct PluginFieldRow: View {
    let field: FormFieldView
    let set: (String) -> Void

    private var text: Binding<String> {
        Binding(get: { field.value }, set: { set($0) })
    }

    var body: some View {
        VStack(alignment: .leading, spacing: 2) {
            control
            if !field.help.isEmpty {
                Text(field.help).font(.caption).foregroundStyle(.secondary)
            }
            if !field.error.isEmpty {
                Text(field.error).font(.caption).foregroundStyle(.red)
            }
        }
    }

    @ViewBuilder
    private var control: some View {
        switch field.kind {
        case .info:
            Text(field.text).foregroundStyle(.secondary).textSelection(.enabled)
        case .paths:
            ForEach(Array(field.paths.enumerated()), id: \.offset) { _, row in
                LabeledContent(row.path, value: row.size).lineLimit(1).truncationMode(.middle)
            }
        case .separator:
            Divider()
        case .group:
            Text(field.label).font(.subheadline.weight(.semibold))
        case .text, .time:
            TextField(field.label, text: text, prompt: Text(field.placeholder))
        case .multiline:
            LabeledContent(field.label) {
                TextEditor(text: text).frame(minHeight: 60)
            }
        case .int, .float:
            number
        case .bool:
            Toggle(field.label, isOn: Binding(
                get: { field.on },
                set: { set($0 ? "true" : "false") }
            ))
        case .choice:
            Picker(field.label, selection: text) {
                ForEach(field.options, id: \.value) { Text($0.label).tag($0.value) }
            }
        case .radio:
            Picker(field.label, selection: text) {
                ForEach(field.options, id: \.value) { Text($0.label).tag($0.value) }
            }
            .pickerStyle(.radioGroup)
        case .path:
            LabeledContent(field.label) {
                HStack {
                    TextField("", text: text, prompt: Text(field.placeholder))
                    Button("Choose…") { choosePath() }
                }
            }
        }
    }

    /// A slider between the bounds when the field asks for one, else a text
    /// field the core reads as a number.
    @ViewBuilder
    private var number: some View {
        if let range = sliderRange {
            LabeledContent(field.label) {
                Slider(value: sliderValue(lowest: range.lowerBound), in: range, step: sliderStep)
                Text(field.value).monospacedDigit().frame(width: 48, alignment: .trailing)
            }
        } else {
            TextField(field.label, text: text, prompt: Text(field.placeholder))
        }
    }

    private var sliderRange: ClosedRange<Double>? {
        guard field.slider, let min = field.minMilli, let max = field.maxMilli, min < max else { return nil }
        let low: Double = Double(min) / 1000
        let high: Double = Double(max) / 1000
        return low ... high
    }

    private var sliderStep: Double {
        if let step = field.stepMilli { return Double(step) / 1000 }
        return field.kind == .int ? 1 : 0.01
    }

    private func sliderValue(lowest: Double) -> Binding<Double> {
        Binding(
            get: { Double(field.value) ?? lowest },
            set: { set(String($0)) }
        )
    }

    /// The panel the field's mode asks for: a file, a folder, or where to save.
    private func choosePath() {
        let panel: NSSavePanel
        if field.pathMode == .save {
            panel = NSSavePanel()
        } else {
            let open = NSOpenPanel()
            open.canChooseDirectories = field.pathMode == .folder
            open.canChooseFiles = field.pathMode == .file
            panel = open
        }
        if !field.value.isEmpty {
            panel.directoryURL = URL(fileURLWithPath: field.value).deletingLastPathComponent()
        }
        if panel.runModal() == .OK, let url = panel.url {
            set(url.path)
        }
    }
}
