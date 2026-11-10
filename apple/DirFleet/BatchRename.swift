import App
import SwiftUI

/// Batch rename, as egui's dialog: find and replace, or a name pattern, with
/// a live preview from the core that flags names that can't be used.
struct BatchRenameSheet: View {
    let core: Core
    let tab: UInt32
    let batch: BatchRenameView
    @State private var fields: BatchFields?

    private var current: BatchFields { fields ?? batch.fields }

    private func binding<T>(_ key: WritableKeyPath<BatchFields, T>) -> Binding<T> {
        Binding(
            get: { current[keyPath: key] },
            set: { value in
                var changed = current
                changed[keyPath: key] = value
                fields = changed
                core.update(.setBatchRename(tab: tab, fields: changed))
            }
        )
    }

    var body: some View {
        VStack(alignment: .leading, spacing: 12) {
            Text("Rename \(batch.items.count) Items").font(.headline)
            Picker("", selection: binding(\.mode)) {
                Text("Find and Replace").tag(BatchMode.replace)
                Text("Name Pattern").tag(BatchMode.pattern)
            }
            .pickerStyle(.segmented)
            .labelsHidden()
            Form {
                switch current.mode {
                case .replace:
                    TextField("Find", text: binding(\.find))
                    TextField("Replace with", text: binding(\.replace))
                case .pattern:
                    TextField("Pattern", text: binding(\.pattern))
                    Stepper("Start at \(current.start)", value: binding(\.start), in: 0 ... 100_000)
                    Text("{name} is the old name, {ext} its extension, {n} a number.")
                        .font(.caption)
                        .foregroundStyle(.secondary)
                }
            }
            List(Array(batch.items.enumerated()), id: \.offset) { _, item in
                HStack {
                    Text(item.old).lineLimit(1)
                    Image(systemName: "arrow.right").foregroundStyle(.secondary)
                    Text(item.new)
                        .lineLimit(1)
                        .fontWeight(item.old == item.new ? .regular : .semibold)
                        .foregroundStyle(item.problem.isEmpty ? Color.primary : Color.red)
                        .help(item.problem)
                }
            }
            .frame(minHeight: 160)
            if batch.problems > 0 {
                Text("\(batch.problems) names can't be used; hover them for details.")
                    .foregroundStyle(.red)
                    .font(.callout)
            }
            HStack {
                Spacer()
                Button("Cancel", role: .cancel) { core.update(.cancelBatchRename(tab: tab)) }
                    .keyboardShortcut(.cancelAction)
                Button("Rename \(batch.changes) Items") { core.update(.applyBatchRename(tab: tab)) }
                    .keyboardShortcut(.defaultAction)
                    .disabled(batch.problems > 0 || batch.changes == 0)
            }
        }
        .padding()
        .frame(width: 520, height: 460)
    }
}
