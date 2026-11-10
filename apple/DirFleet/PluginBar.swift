import App
import SwiftUI

/// Plugins' segments of the bar below a folder, as egui's directory bar:
/// text, badges, buttons that run the plugin's actions, spinners, each in
/// its tone.
struct PluginBar: View {
    let core: Core
    let tab: TabViewModel

    var body: some View {
        HStack(spacing: 14) {
            ForEach(Array(tab.pluginBar.enumerated()), id: \.offset) { _, bar in
                HStack(spacing: 6) {
                    if !bar.error.isEmpty {
                        Label(bar.plugin, systemImage: "exclamationmark.triangle")
                            .foregroundStyle(.red)
                            .help(bar.error)
                    } else if bar.segments.isEmpty, bar.loading {
                        ProgressView().controlSize(.mini)
                    }
                    ForEach(Array(bar.segments.enumerated()), id: \.offset) { _, segment in
                        SegmentView(core: core, tab: tab.id, segment: segment)
                    }
                }
                .help(bar.plugin)
            }
            Spacer()
        }
        .font(.callout)
        .padding(.horizontal, 10)
        .padding(.vertical, 4)
        .background(.bar)
    }
}

private struct SegmentView: View {
    let core: Core
    let tab: UInt32
    let segment: BarSegmentView

    private var tone: Color {
        switch segment.tone {
        case "good": .green
        case "warn": .orange
        case "bad": .red
        case "muted": .secondary
        default: .primary
        }
    }

    var body: some View {
        HStack(spacing: 4) {
            if segment.spinner { ProgressView().controlSize(.mini) }
            if !segment.text.isEmpty { Text(segment.text).foregroundStyle(tone) }
            if !segment.badge.isEmpty {
                Text(segment.badge)
                    .font(.caption.weight(.semibold))
                    .padding(.horizontal, 6)
                    .background(tone.opacity(0.18), in: Capsule())
                    .foregroundStyle(tone)
            }
            if !segment.button.isEmpty {
                Button(segment.button) {
                    if !segment.action.isEmpty {
                        core.update(.runPluginAction(tab: tab, action: segment.action))
                    }
                }
                .controlSize(.small)
                .disabled(segment.action.isEmpty)
            }
        }
        .help(segment.tooltip)
    }
}
