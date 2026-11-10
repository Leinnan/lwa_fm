import App
import SwiftUI

/// As Finder's status bar and egui's: how many items, how many selected,
/// the drive's free space (its details on hover) and an eject button.
struct StatusBar: View {
    let core: Core
    let store: TabStore

    var body: some View {
        // Its own part of the tab: drawn again only when a number changes.
        let tab = store.counts
        HStack(spacing: 8) {
            Spacer()
            Text(summary(tab))
            if let drive = tab.driveStatus {
                if !drive.free.isEmpty {
                    Text(drive.free).help(drive.detail)
                }
                if let eject = drive.eject {
                    Button {
                        core.update(.ejectDrive(mount: drive.mount))
                    } label: {
                        Image(systemName: "eject")
                    }
                    .buttonStyle(.borderless)
                    .help(eject)
                }
            }
            Spacer()
        }
        .font(.caption)
        .foregroundStyle(.secondary)
        .padding(.vertical, 3)
        .background(.bar)
    }

    private func summary(_ tab: TabCounts) -> String {
        let items = tab.totalRows == 1 ? "1 item" : "\(tab.totalRows) items"
        return tab.selectedCount > 0 ? "\(tab.selectedCount) of \(items) selected" : items
    }
}
