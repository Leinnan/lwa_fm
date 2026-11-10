import App
import SwiftUI

/// As egui's banner: while the app can't read every folder, offer System
/// Settings, then wait for the grant, then offer to restart.
struct DiskAccessBanner: View {
    let core: Core

    var body: some View {
        HStack(spacing: 8) {
            Image(systemName: "lock.shield").foregroundStyle(.secondary)
            Text(message).lineLimit(2)
            Spacer()
            switch core.view.access.prompt {
            case .restart:
                Button("Later") { core.update(.dismissDiskAccess(forever: false)) }
                Button("Restart DirFleet") { core.update(.restartApp) }
                    .buttonStyle(.borderedProminent)
            case .waiting:
                ProgressView().controlSize(.small)
                Button("Not now") { core.update(.dismissDiskAccess(forever: false)) }
            case .ask, .hidden:
                Button("Don't Ask Again") { core.update(.dismissDiskAccess(forever: true)) }
                Button("Not now") { core.update(.dismissDiskAccess(forever: false)) }
                Button("Open System Settings") { core.update(.openDiskAccessSettings) }
                    .buttonStyle(.borderedProminent)
            }
        }
        .controlSize(.small)
        .padding(.horizontal, 12)
        .padding(.vertical, 6)
        .background(.bar)
    }

    private var message: String {
        switch core.view.access.prompt {
        case .restart: "Full Disk Access is on. Restart DirFleet to use it."
        case .waiting: "Turn on DirFleet under Full Disk Access in System Settings."
        case .ask, .hidden:
            "Give DirFleet Full Disk Access to browse every folder without being asked for permission."
        }
    }
}
