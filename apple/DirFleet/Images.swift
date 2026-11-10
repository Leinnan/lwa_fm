import AppKit
import SwiftUI
import UniformTypeIdentifiers

/// Thumbnail files the core made, for SwiftUI views that show one at a
/// time. The folder's rows load theirs through `ThumbnailLoader`, whose
/// cache this shares.
@MainActor
enum Thumbnails {
    /// Decoded now, on the calling thread: for views that can't wait, such
    /// as the thumbnail picker's few frames.
    static func image(at file: String) -> NSImage? {
        if let cached = ThumbnailLoader.shared.cached(file) {
            return NSImage(cgImage: cached, size: .zero)
        }
        guard let image = ThumbnailLoader.decodeFile(file) else { return nil }
        ThumbnailLoader.shared.keep(image, for: file)
        return NSImage(cgImage: image, size: .zero)
    }
}

/// Finder's icons for files, by type: one lookup serves every PNG.
enum FileIcons {
    private static let byType = NSCache<NSString, NSImage>()

    static func icon(for path: String, isDir: Bool) -> NSImage {
        let ext = (path as NSString).pathExtension.lowercased()
        let key = (isDir ? "dir:" : "file:") + ext
        if let cached = byType.object(forKey: key as NSString) {
            return cached
        }
        let type = isDir
            ? (ext.isEmpty ? UTType.folder : UTType(filenameExtension: ext, conformingTo: .directory) ?? .folder)
            : (UTType(filenameExtension: ext) ?? .data)
        let icon = NSWorkspace.shared.icon(for: type)
        byType.setObject(icon, forKey: key as NSString)
        return icon
    }
}
