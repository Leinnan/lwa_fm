import App
import SwiftUI

/// A plugin action's results dialog, as egui's: the options, the size
/// filter, progress, and the table the plugin's job fills in. Running in the
/// background hides it; the job's progress line brings it back.
struct PluginResultsSheet: View {
    let core: Core
    let results: PluginResultsView

    /// The width of every column after the first, as egui's.
    private let valueWidth: CGFloat = 96

    var body: some View {
        VStack(alignment: .leading, spacing: 10) {
            Text(results.title).font(.title3.weight(.semibold))
            toolbar
            if !results.message.isEmpty {
                Text(results.message).foregroundStyle(.red).textSelection(.enabled)
            }
            if results.running {
                HStack {
                    ProgressView(value: Double(results.progress), total: 1000).frame(width: 160)
                    Text(results.progressText).foregroundStyle(.secondary).lineLimit(1)
                }
            }
            if !results.note.isEmpty {
                Text(results.note).foregroundStyle(.secondary)
            }
            if results.hasTable {
                HStack {
                    Text(results.subtitle).font(.caption).foregroundStyle(.secondary)
                    Spacer()
                    Text(results.summary).font(.caption).foregroundStyle(.secondary)
                }
            }
            table.frame(minHeight: 260)
            buttons
        }
        .padding()
        .frame(width: 760, height: 560)
    }

    private var toolbar: some View {
        HStack(alignment: .firstTextBaseline, spacing: 16) {
            ForEach(results.fields, id: \.id) { field in
                PluginFieldRow(field: field) { core.update(.setPluginResultsField(id: field.id, text: $0)) }
                    .frame(maxWidth: 260)
                    .disabled(!field.enabled)
            }
            Spacer()
            if results.hasSizes {
                Text("Hide smaller than")
                TextField("0", text: Binding(
                    get: { results.filterAmount },
                    set: { core.update(.filterPluginResults(amount: $0, gigabytes: results.filterGigabytes)) }
                ))
                .frame(width: 56)
                Picker("Size unit", selection: Binding(
                    get: { results.filterGigabytes },
                    set: { core.update(.filterPluginResults(amount: results.filterAmount, gigabytes: $0)) }
                )) {
                    Text("MB").tag(false)
                    Text("GB").tag(true)
                }
                .labelsHidden()
                .frame(width: 64)
                if !results.filterText.isEmpty {
                    Text(results.filterText).font(.caption).foregroundStyle(.secondary)
                }
            }
        }
    }

    @ViewBuilder
    private var table: some View {
        if results.rows.isEmpty {
            Text(results.emptyText)
                .foregroundStyle(.secondary)
                .frame(maxWidth: .infinity, maxHeight: .infinity)
        } else {
            VStack(spacing: 0) {
                header
                Divider()
                List {
                    ForEach(results.rows, id: \.path) { row in
                        rowView(row)
                    }
                }
                .listStyle(.plain)
            }
        }
    }

    private var header: some View {
        HStack(spacing: 0) {
            ForEach(Array(results.columns.enumerated()), id: \.offset) { index, column in
                Button {
                    core.update(.sortPluginResults(column: UInt32(index)))
                } label: {
                    HStack(spacing: 4) {
                        Text(column.title)
                        if column.sorted {
                            Image(systemName: column.descending ? "chevron.down" : "chevron.up")
                        }
                    }
                    .font(.caption)
                    .foregroundStyle(.secondary)
                    .frame(maxWidth: index == 0 ? .infinity : valueWidth, alignment: index == 0 ? .leading : .trailing)
                }
                .buttonStyle(.plain)
                .frame(width: index == 0 ? nil : valueWidth)
            }
        }
        .padding(.horizontal, 16)
        .padding(.vertical, 4)
    }

    private func rowView(_ row: ResultsRowView) -> some View {
        HStack(spacing: 0) {
            ForEach(Array(row.cells.enumerated()), id: \.offset) { index, cell in
                if index == 0 {
                    HStack(spacing: 6) {
                        Text(cell).lineLimit(1).truncationMode(.middle)
                        if !row.status.isEmpty {
                            if row.busy {
                                ProgressView().controlSize(.mini)
                            } else {
                                Image(systemName: "exclamationmark.triangle.fill").foregroundStyle(.red)
                            }
                            Text(row.status).font(.caption).foregroundStyle(row.busy ? Color.secondary : .red)
                        }
                        Spacer(minLength: 0)
                    }
                    .frame(maxWidth: .infinity, alignment: .leading)
                } else {
                    Text(cell).monospacedDigit().frame(width: valueWidth, alignment: .trailing)
                }
            }
        }
        .contentShape(Rectangle())
        .help(row.failure.isEmpty ? row.tooltip : row.failure)
        .onTapGesture(count: 2) {
            if !row.path.isEmpty { core.update(.openPluginResultsRow(path: row.path)) }
        }
        .contextMenu {
            if !row.path.isEmpty {
                Button(results.openLabel) { core.update(.openPluginResultsRow(path: row.path)) }
                if !results.refreshLabel.isEmpty {
                    Button(results.refreshLabel) { core.update(.refreshPluginResultsRow(path: row.path)) }
                        .disabled(!row.actionsEnabled)
                }
                if !row.menu.isEmpty {
                    Divider()
                    ForEach(row.menu, id: \.action) { item in
                        Button(item.label, role: item.danger ? .destructive : nil) {
                            core.update(.pluginResultsRowAction(path: row.path, action: item.action))
                        }
                        .disabled(!row.actionsEnabled)
                    }
                }
                Divider()
                Button(results.copyLabel) { core.update(.copyPath(path: row.path)) }
            }
        }
    }

    private var buttons: some View {
        HStack {
            if results.busy { ProgressView().controlSize(.small) }
            Spacer()
            if results.running {
                Button("Run in Background") { core.update(.hidePluginResults) }
                Button("Stop") { core.update(.stopPluginResults) }
                Button("Restart") { core.update(.runPluginResults) }
                    .disabled(results.busy)
            } else {
                Button("Close") { core.update(.hidePluginResults) }
                    .keyboardShortcut(.cancelAction)
                Button("Run") { core.update(.runPluginResults) }
                    .keyboardShortcut(.defaultAction)
                    .disabled(results.busy)
            }
        }
    }
}
