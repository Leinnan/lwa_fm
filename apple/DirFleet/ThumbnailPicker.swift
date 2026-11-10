import App
import SwiftUI

/// "Choose Thumbnail", as egui's dialog: the selected frame large beside a
/// sheet of the video's frames in time order, with a + between two frames to
/// add more there. The arrow keys move over finished frames, Return uses the
/// selected one. Everything it shows comes from the core.
struct ThumbnailPickerSheet: View {
    let core: Core
    let picker: ThumbnailPickerView
    /// Frames per row of the sheet, for the up and down arrows.
    @State private var columns = 3

    private let cell: CGFloat = 140

    var body: some View {
        VStack(alignment: .leading, spacing: 12) {
            HStack(alignment: .firstTextBaseline) {
                Text("Choose Thumbnail").font(.headline)
                Text(picker.name).foregroundStyle(.secondary).lineLimit(1).truncationMode(.middle)
                Spacer()
                Text(picker.duration).foregroundStyle(.secondary).monospacedDigit()
            }
            if picker.error.isEmpty {
                HStack(alignment: .top, spacing: 16) {
                    preview.frame(width: 340)
                    frames
                }
            } else {
                Text(picker.error).frame(maxWidth: .infinity, maxHeight: .infinity)
            }
            HStack {
                VStack(alignment: .leading) {
                    ForEach([picker.status, picker.failed], id: \.self) { line in
                        if !line.isEmpty { Text(line) }
                    }
                    if picker.atLimit { Text("Frame limit reached: close the dialog to start over") }
                }
                .font(.callout)
                .foregroundStyle(.secondary)
                Spacer()
                Button("Cancel", role: .cancel) { core.update(.closePicker) }
                    .keyboardShortcut(.cancelAction)
                Button("Reset to Default") { core.update(.resetPickerFrame) }
                    .disabled(!picker.canReset)
                Button("Use Frame") { core.update(.usePickerFrame) }
                    .keyboardShortcut(.defaultAction)
                    .disabled(!picker.canApply)
            }
        }
        .padding()
        .frame(width: 860, height: 540)
        .focusable()
        .focusEffectDisabled()
        .onKeyPress(.leftArrow) { step(forward: false, steps: 1) }
        .onKeyPress(.rightArrow) { step(forward: true, steps: 1) }
        .onKeyPress(.upArrow) { step(forward: false, steps: columns) }
        .onKeyPress(.downArrow) { step(forward: true, steps: columns) }
    }

    private func step(forward: Bool, steps: Int) -> KeyPress.Result {
        core.update(.stepPickerFrame(forward: forward, steps: UInt32(max(steps, 1))))
        return .handled
    }

    private var preview: some View {
        VStack(alignment: .leading, spacing: 8) {
            FrameImage(file: picker.previewImage)
                .aspectRatio(16 / 9, contentMode: .fit)
            HStack(alignment: .firstTextBaseline) {
                Text(picker.previewLabel).font(.title2).monospacedDigit()
                Text(picker.previewBadge).foregroundStyle(.secondary)
            }
            HStack {
                Button("‹ Previous") { _ = step(forward: false, steps: 1) }.disabled(!picker.previous)
                Button("Next ›") { _ = step(forward: true, steps: 1) }.disabled(!picker.next)
            }
            HStack {
                Text("More frames:").foregroundStyle(.secondary)
                gapButton("‹ Before", picker.moreBefore)
                gapButton("After ›", picker.moreAfter)
            }
        }
    }

    private func gapButton(_ title: String, _ gap: PickerGapView?) -> some View {
        Button(title) {
            if let gap { core.update(.fillPickerGap(from: gap.from, to: gap.to)) }
        }
        .disabled(gap == nil)
        .help(gap?.tooltip ?? "")
    }

    private var frames: some View {
        GeometryReader { geometry in
            ScrollViewReader { scroller in
                ScrollView { grid }
                    // The sheet follows the selection the arrow keys move.
                    .onChange(of: selectedFrame) { _, selected in
                        if let selected { withAnimation { scroller.scrollTo(selected) } }
                    }
            }
            .onAppear { columns = columnCount(geometry.size.width) }
            .onChange(of: geometry.size.width) { _, width in columns = columnCount(width) }
        }
    }

    private var selectedFrame: UInt64? {
        picker.frames.first(where: \.selected)?.atMs
    }

    private func columnCount(_ width: CGFloat) -> Int {
        max(1, Int((width - 24 + 8) / (cell + 8)))
    }

    private var grid: some View {
        LazyVGrid(columns: [GridItem(.adaptive(minimum: cell), spacing: 8)], spacing: 8) {
            ForEach(picker.frames, id: \.atMs) { frame in tile(frame) }
        }
        .padding(.horizontal, 12)
    }

    private func tile(_ frame: PickerFrameView) -> some View {
        FrameCell(frame: frame)
            .id(frame.atMs)
            .onTapGesture(count: 2) {
                core.update(.selectPickerFrame(atMs: frame.atMs))
                core.update(.usePickerFrame)
            }
            .onTapGesture { core.update(.selectPickerFrame(atMs: frame.atMs)) }
            .overlay(alignment: .trailing) { more(frame.gapAfter) }
    }

    /// The + that adds frames between this one and the next.
    @ViewBuilder
    private func more(_ gap: PickerGapView?) -> some View {
        if let gap {
            Button { core.update(.fillPickerGap(from: gap.from, to: gap.to)) } label: {
                Image(systemName: "plus.circle.fill")
            }
            .buttonStyle(.plain)
            .foregroundStyle(.secondary)
            .help(gap.tooltip)
            .accessibilityLabel(gap.tooltip)
            .offset(x: 10)
        }
    }
}

/// A frame of the sheet: its picture, time and badge, outlined when selected.
private struct FrameCell: View {
    let frame: PickerFrameView

    var body: some View {
        VStack(spacing: 4) {
            FrameImage(file: frame.image)
                .aspectRatio(16 / 9, contentMode: .fit)
                .overlay(alignment: .topLeading) {
                    if !frame.badge.isEmpty {
                        Text(frame.badge)
                            .font(.caption2)
                            .padding(.horizontal, 4)
                            .background(.black.opacity(0.7), in: RoundedRectangle(cornerRadius: 3))
                            .foregroundStyle(.white)
                            .padding(4)
                    }
                }
                .overlay {
                    RoundedRectangle(cornerRadius: 4)
                        .strokeBorder(frame.selected ? Color.accentColor : .clear, lineWidth: 2)
                }
            Text(frame.label).font(.caption).monospacedDigit().foregroundStyle(.secondary)
        }
        .contentShape(Rectangle())
        .accessibilityElement(children: .combine)
        .accessibilityLabel("Frame at \(frame.label)")
        .accessibilityAddTraits(frame.selected ? [.isButton, .isSelected] : .isButton)
    }
}

/// A frame's picture, or a spinner while it is extracted.
private struct FrameImage: View {
    let file: String

    var body: some View {
        ZStack {
            RoundedRectangle(cornerRadius: 4).fill(.quaternary)
            if let image = Thumbnails.image(at: file) {
                Image(nsImage: image).resizable().scaledToFit()
            } else {
                ProgressView().controlSize(.small)
            }
        }
        .clipShape(RoundedRectangle(cornerRadius: 4))
    }
}
