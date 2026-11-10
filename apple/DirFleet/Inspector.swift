import App
import QuickLookThumbnailing
import SwiftUI

/// The inspector: the selected item's preview and details, or a summary of
/// several. Folder sizes are added up on request, as in egui.
struct InspectorPane: View {
    let core: Core
    let store: TabStore

    var body: some View {
        // Its own part of the tab: a click that keeps the selection, a
        // scroll or a thumbnail leaves it alone.
        let tab = store.inspector
        Group {
            if let item = tab.inspector?.item {
                ItemDetails(core: core, tab: tab, item: item)
            } else if let summary = tab.inspector?.summary {
                SummaryDetails(core: core, tab: tab, summary: summary)
            } else {
                ContentUnavailableView("No Selection", systemImage: "info.circle")
            }
        }
        .inspectorColumnWidth(min: 220, ideal: 260, max: 400)
    }
}

private struct ItemDetails: View {
    let core: Core
    let tab: InspectorSlice
    let item: ItemInfo

    var body: some View {
        Form {
            Section {
                PreviewCard(core: core, item: item)
                    .frame(maxWidth: .infinity)
                    .frame(height: 180)
                Text(item.name).font(.headline).textSelection(.enabled)
                Text(item.path).font(.caption).foregroundStyle(.secondary).textSelection(.enabled)
            }
            Section("Information") {
                LabeledContent("Type", value: item.kind)
                LabeledContent("Size") {
                    switch item.size {
                    case let .known(size): Text(size)
                    case .notCalculated:
                        Button("Calculate") { core.update(.calculateSize(path: item.path)) }
                    case .calculating: ProgressView().controlSize(.small)
                    case .unavailable: Text("Unavailable").foregroundStyle(.secondary)
                    }
                }
                LabeledContent("Created", value: item.created)
                LabeledContent("Modified", value: item.modified)
                LabeledContent("Opened", value: item.opened)
                if item.opens > 0 {
                    LabeledContent("Opened here", value: item.opens == 1 ? "Once" : "\(item.opens) times")
                }
            }
            PluginSections(sections: tab.pluginSections)
            Section {
                HStack {
                    Button("Open") { core.update(.openSelection(tab: tab.tab)) }
                    Button("Copy as Pathname") { core.update(.copyPaths(tab: tab.tab)) }
                    Button("Show in Finder") { core.update(.revealSelection(tab: tab.tab)) }
                }
            }
        }
        .formStyle(.grouped)
    }
}

private struct SummaryDetails: View {
    let core: Core
    let tab: InspectorSlice
    let summary: SelectionSummary

    var body: some View {
        Form {
            Section("Selection") {
                Text("\(summary.count) items selected").font(.headline)
                LabeledContent("Folders", value: "\(summary.folders)")
                LabeledContent("Files", value: "\(summary.files)")
                if summary.files > 0 {
                    LabeledContent("Files total", value: summary.total)
                }
            }
            Section("By Kind") {
                ForEach(summary.kinds, id: \.kind) { kind in
                    LabeledContent(kind.kind, value: "\(kind.count)")
                }
            }
            PluginSections(sections: tab.pluginSections)
            Section {
                HStack {
                    Button("Copy") { core.update(.copySelection(tab: tab.tab)) }
                    Button("Duplicate") { core.update(.duplicate(tab: tab.tab)) }
                    Button("Move to \(core.view.trash.label)") { core.trashSelection(tab: tab.tab) }
                        .disabled(tab.inTrash)
                }
            }
        }
        .formStyle(.grouped)
    }
}

/// What plugins add to the inspector for the selection, as egui's sections.
private struct PluginSections: View {
    let sections: [PluginSectionView]

    var body: some View {
        ForEach(sections, id: \.title) { section in
            Section(section.title) {
                if !section.error.isEmpty {
                    Text(section.error).font(.caption).foregroundStyle(.red)
                } else if section.rows.isEmpty, section.loading {
                    ProgressView().controlSize(.small)
                }
                ForEach(Array(section.rows.enumerated()), id: \.offset) { _, row in
                    LabeledContent(row.label) {
                        Text(row.value).textSelection(.enabled)
                    }
                }
            }
        }
    }
}

/// The item's preview: the core's thumbnail for images and videos, with
/// loading and unavailable states, or Quick Look's for other files (PDFs,
/// documents), with the file's icon until one is ready.
private struct PreviewCard: View {
    let core: Core
    let item: ItemInfo

    var body: some View {
        if !item.animation.isEmpty {
            AnimatedImage(file: item.animation)
        } else {
            still
        }
    }

    @ViewBuilder private var still: some View {
        switch item.preview {
        case let .ready(file):
            if let image = Thumbnails.image(at: file) {
                Image(nsImage: image).resizable().scaledToFit()
            } else {
                icon
            }
        case .loading:
            icon.overlay { ProgressView().controlSize(.small) }
        case .unavailable:
            VStack(spacing: 8) {
                icon.frame(height: 96)
                Text("No preview").font(.caption).foregroundStyle(.secondary)
                Button("Try Again") { core.update(.retryPreview(path: item.path)) }
                    .controlSize(.small)
            }
        case .none:
            QuickLookThumbnail(path: item.path)
        }
    }

    private var icon: some View {
        Image(nsImage: FileIcons.icon(for: item.path, isDir: item.isDir)).resizable().scaledToFit().padding(24)
    }
}

/// A Quick Look thumbnail of `path`, or its icon until one is ready.
private struct QuickLookThumbnail: View {
    let path: String
    @State private var image: NSImage?

    var body: some View {
        Group {
            if let image {
                Image(nsImage: image).resizable().scaledToFit()
            } else {
                Image(nsImage: FileIcons.icon(for: path, isDir: false)).resizable().scaledToFit().padding(24)
            }
        }
        .task(id: path) {
            image = nil
            let request = QLThumbnailGenerator.Request(
                fileAt: URL(fileURLWithPath: path),
                size: CGSize(width: 360, height: 360),
                scale: NSScreen.main?.backingScaleFactor ?? 2,
                representationTypes: .thumbnail
            )
            image = try? await QLThumbnailGenerator.shared.generateBestRepresentation(for: request).nsImage
        }
    }
}
