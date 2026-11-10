import AppKit
import ImageIO
import UniformTypeIdentifiers

/// Decodes the core's thumbnail files off the main thread: each file once
/// however many cells ask for it, the newest request first (what scrolled
/// into sight last is what the user looks at), a few at a time. A cell that
/// is reused cancels its request; one nobody waits for is never started.
@MainActor
final class ThumbnailLoader {
    static let shared = ThumbnailLoader()

    /// A request for one file by one cell, to cancel it with.
    final class Ticket {
        fileprivate let file: String
        fileprivate var done: ((CGImage?) -> Void)?

        fileprivate init(file: String, done: @escaping (CGImage?) -> Void) {
            self.file = file
            self.done = done
        }
    }

    /// Decoded pixels, kept while memory allows: a few hundred tiles at
    /// grid size.
    private let cache: NSCache<NSString, CGImage> = {
        let cache = NSCache<NSString, CGImage>()
        cache.totalCostLimit = 256 * 1024 * 1024
        return cache
    }()
    /// Who waits for each file being decoded or about to be.
    private var waiting: [String: [Ticket]] = [:]
    /// Files not started yet; the last one is decoded next.
    private var pending: [String] = []
    private var running = 0
    private let limit: Int
    private let queue = DispatchQueue(label: "io.github.leinnan.dirfleet.thumbnails", qos: .userInitiated, attributes: .concurrent)
    /// Reads and decodes a file; tests put their own here.
    var decode: @Sendable (String) -> CGImage? = { ThumbnailLoader.decodeFile($0) }

    init(limit: Int = max(2, ProcessInfo.processInfo.activeProcessorCount / 2)) {
        self.limit = limit
    }

    /// The image for `file`, if it was decoded before.
    func cached(_ file: String) -> CGImage? {
        file.isEmpty ? nil : cache.object(forKey: file as NSString)
    }

    /// Keeps an image decoded elsewhere.
    func keep(_ image: CGImage, for file: String) {
        cache.setObject(image, forKey: file as NSString, cost: image.bytesPerRow * image.height)
    }

    /// Decodes `file` and calls `done` on the main thread, unless the
    /// ticket is cancelled first. Ask `cached` before: this always waits.
    func load(_ file: String, _ done: @escaping (CGImage?) -> Void) -> Ticket {
        let ticket = Ticket(file: file, done: done)
        if waiting[file] != nil {
            waiting[file]?.append(ticket)
            // Asked for again: it goes to the front of the line.
            if let index = pending.lastIndex(of: file), index != pending.count - 1 {
                pending.remove(at: index)
                pending.append(file)
            }
        } else {
            waiting[file] = [ticket]
            pending.append(file)
            start()
        }
        return ticket
    }

    /// Forgets a request. A file nobody waits for any more isn't decoded,
    /// unless its decoding has begun.
    func cancel(_ ticket: Ticket) {
        ticket.done = nil
        guard var tickets = waiting[ticket.file] else { return }
        tickets.removeAll { $0 === ticket }
        if tickets.isEmpty, let index = pending.lastIndex(of: ticket.file) {
            pending.remove(at: index)
            waiting[ticket.file] = nil
        } else {
            waiting[ticket.file] = tickets
        }
    }

    /// How many files wait or are being decoded, for tests.
    var outstanding: Int { waiting.count }

    private func start() {
        while running < limit, let file = pending.popLast() {
            running += 1
            let decode = decode
            queue.async {
                let image = decode(file)
                DispatchQueue.main.async {
                    MainActor.assumeIsolated { self.finished(file, image) }
                }
            }
        }
    }

    private func finished(_ file: String, _ image: CGImage?) {
        running -= 1
        if let image { keep(image, for: file) }
        for ticket in waiting.removeValue(forKey: file) ?? [] {
            ticket.done?(image)
            ticket.done = nil
        }
        start()
    }

    /// Decoded fully up front, so drawing the image later costs nothing.
    nonisolated static func decodeFile(_ file: String) -> CGImage? {
        let options = [kCGImageSourceShouldCache: false] as CFDictionary
        guard let source = CGImageSourceCreateWithURL(URL(fileURLWithPath: file) as CFURL, options) else {
            return nil
        }
        return CGImageSourceCreateImageAtIndex(source, 0, [kCGImageSourceShouldCacheImmediately: true] as CFDictionary)
    }
}

/// Finder's icons as bitmaps at the size they are drawn, so a cell sets one
/// as its layer's contents and nothing is drawn. Ordinary files share their
/// type's icon; folders, packages and apps can have their own, which are
/// looked up by path off the main thread.
@MainActor
enum IconCache {
    private static let byType = NSCache<NSString, CGImage>()
    private static let byPath: NSCache<NSString, CGImage> = {
        let cache = NSCache<NSString, CGImage>()
        cache.countLimit = 2000
        return cache
    }()
    private static var looking: [String: [(CGImage) -> Void]] = [:]

    /// An icon to show at once: the path's own when it was looked up
    /// before, else its type's.
    static func icon(for path: String, isDir: Bool, pixels: Int) -> CGImage? {
        if let own = byPath.object(forKey: "\(pixels):\(path)" as NSString) {
            return own
        }
        let ext = (path as NSString).pathExtension.lowercased()
        let key = "\(pixels):\(isDir ? "dir" : "file"):\(ext)" as NSString
        if let cached = byType.object(forKey: key) {
            return cached
        }
        let type = isDir
            ? (ext.isEmpty ? UTType.folder : UTType(filenameExtension: ext, conformingTo: .directory) ?? .folder)
            : (UTType(filenameExtension: ext) ?? .data)
        guard let image = bitmap(NSWorkspace.shared.icon(for: type), pixels: pixels) else { return nil }
        byType.setObject(image, forKey: key)
        return image
    }

    /// Whether the file can have an icon of its own.
    static func needsOwnIcon(_ path: String, isDir: Bool) -> Bool {
        isDir || (path as NSString).pathExtension.isEmpty
    }

    /// Looks the path's own icon up off the main thread, once, and calls
    /// `done` with it on the main thread unless it is cached already
    /// (`icon` returns it then).
    static func ownIcon(for path: String, pixels: Int, _ done: @escaping (CGImage) -> Void) {
        let key = "\(pixels):\(path)"
        if byPath.object(forKey: key as NSString) != nil { return }
        if looking[key] != nil {
            looking[key]?.append(done)
            return
        }
        looking[key] = [done]
        DispatchQueue.global(qos: .utility).async {
            let image = bitmap(NSWorkspace.shared.icon(forFile: path), pixels: pixels)
            DispatchQueue.main.async {
                MainActor.assumeIsolated {
                    let waiters = looking.removeValue(forKey: key) ?? []
                    guard let image else { return }
                    byPath.setObject(image, forKey: key as NSString)
                    for waiter in waiters { waiter(image) }
                }
            }
        }
    }

    nonisolated private static func bitmap(_ icon: NSImage, pixels: Int) -> CGImage? {
        var rect = CGRect(x: 0, y: 0, width: pixels, height: pixels)
        return icon.cgImage(forProposedRect: &rect, context: nil, hints: nil)
    }
}
