import App
import AppKit
import SwiftUI
import XCTest


/// How long the main thread went without running its run loop.
@MainActor
private final class Heartbeat {
    private var timer: Timer?
    private var last = ContinuousClock.now
    private(set) var longest = Duration.zero
    /// Stalls over 50 ms, which a user sees as a hitch.
    private(set) var stalls = 0

    func start() {
        last = .now
        longest = .zero
        stalls = 0
        let timer = Timer(timeInterval: 0.004, repeats: true) { [weak self] _ in
            MainActor.assumeIsolated { self?.beat() }
        }
        RunLoop.main.add(timer, forMode: .common)
        self.timer = timer
    }

    func stop() {
        timer?.invalidate()
        timer = nil
    }

    private func beat() {
        let now = ContinuousClock.now
        let gap = now - last
        last = now
        longest = max(longest, gap)
        if gap > .milliseconds(50) { stalls += 1 }
    }
}

/// The Mac app on a folder shaped like a busy Desktop (847 entries, mostly
/// screenshots and screen recordings), drawn in a real window: what the
/// bridge and SwiftUI cost while opening, clicking, scrolling and idling.
/// Prints one `PERF` line per scenario; run with `just apple-perf`, which
/// builds the fixture and runs these alone, so the thumbnail cache is theirs.
@MainActor
final class PerfTests: XCTestCase {
    private var window: NSWindow?
    private let heartbeat = Heartbeat()
    private var lines: [String] = []

    override func setUp() {
        super.setUp()
        setenv("DIRFLEET_NO_DOWNLOADS", "1", 1)
    }

    override func tearDown() {
        window?.orderOut(nil)
        window = nil
        super.tearDown()
    }

    private static let stateDir = URL(fileURLWithPath: NSTemporaryDirectory())
        .appendingPathComponent("dirfleet_perf_state")
    /// Where the effects keep thumbnails for a state file in `stateDir`.
    private static let cacheDir = FileManager.default.urls(for: .cachesDirectory, in: .userDomainMask)[0]
        .appendingPathComponent("dirfleet_perf_state")

    private func fixture() throws -> String {
        guard ProcessInfo.processInfo.environment["DIRFLEET_PERF"] == "1" else {
            throw XCTSkip("performance runs only with `just apple-perf`")
        }
        let dir = URL(fileURLWithPath: NSTemporaryDirectory()).appendingPathComponent("dirfleet_perf_desktop")
        guard FileManager.default.fileExists(atPath: dir.appendingPathComponent(".complete").path) else {
            throw XCTSkip("run scripts/desktop_like_fixture.sh first")
        }
        return dir.path
    }

    /// A flat folder of `count` one-byte text files, made once: the one the
    /// Rust latency test (`crates/dirfleet_ffi/tests/latency.rs`) uses.
    private func flatFixture(_ count: Int) throws -> String {
        guard ProcessInfo.processInfo.environment["DIRFLEET_PERF"] == "1" else {
            throw XCTSkip("performance runs only with `just apple-perf`")
        }
        let fm = FileManager.default
        let dir = URL(fileURLWithPath: NSTemporaryDirectory()).appendingPathComponent("lwa_fm_perf_flat_\(count)")
        let marker = dir.appendingPathComponent(".complete")
        if !fm.fileExists(atPath: marker.path) {
            try? fm.removeItem(at: dir)
            try fm.createDirectory(at: dir, withIntermediateDirectories: true)
            let content = Data("x".utf8)
            for index in 0..<count {
                try content.write(to: dir.appendingPathComponent(String(format: "file_%06d.txt", index)))
            }
            try Data().write(to: marker)
        }
        return dir.path
    }

    /// Draws what changed, as the next frame would, so a time ends when the
    /// change is on screen and not when the view model has it.
    private func draw() {
        window?.contentView?.layoutSubtreeIfNeeded()
        window?.displayIfNeeded()
        CATransaction.flush()
    }

    /// Runs `act`; milliseconds until `done`, and until that is drawn. Nil
    /// when `done` didn't come within `timeout` seconds.
    private func time(timeout: Double = 120, _ act: () -> Void, until done: () -> Bool) async throws -> (model: Double, drawn: Double)? {
        let start = ContinuousClock.now
        act()
        while !done() {
            if (ContinuousClock.now - start).seconds > timeout { return nil }
            try await Task.sleep(for: .milliseconds(1))
        }
        let model = (ContinuousClock.now - start).seconds
        draw()
        return (model * 1000, (ContinuousClock.now - start).seconds * 1000)
    }

    /// CPU seconds the process has used, its live threads and its memory
    /// footprint in megabytes.
    private func processStats() -> (cpu: Double, threads: Int, footprint: Double) {
        var usage = rusage()
        getrusage(RUSAGE_SELF, &usage)
        let cpu = Double(usage.ru_utime.tv_sec + usage.ru_stime.tv_sec)
            + Double(usage.ru_utime.tv_usec + usage.ru_stime.tv_usec) / 1e6
        var threads: thread_act_array_t?
        var count = mach_msg_type_number_t(0)
        if task_threads(mach_task_self_, &threads, &count) == KERN_SUCCESS, let threads {
            for index in 0..<Int(count) { mach_port_deallocate(mach_task_self_, threads[index]) }
            vm_deallocate(mach_task_self_, vm_address_t(UInt(bitPattern: threads)),
                          vm_size_t(Int(count) * MemoryLayout<thread_act_t>.stride))
        }
        var info = task_vm_info_data_t()
        var size = mach_msg_type_number_t(MemoryLayout<task_vm_info_data_t>.size / MemoryLayout<natural_t>.size)
        let read = withUnsafeMutablePointer(to: &info) { pointer in
            pointer.withMemoryRebound(to: integer_t.self, capacity: Int(size)) {
                task_info(mach_task_self_, task_flavor_t(TASK_VM_INFO), $0, &size)
            }
        }
        let footprint = read == KERN_SUCCESS ? Double(info.phys_footprint) / 1_048_576 : 0
        return (cpu, Int(count), footprint)
    }

    /// How busy the machine is: numbers taken while other work runs mean little.
    private func reportLoad() {
        var load = [Double](repeating: 0, count: 3)
        getloadavg(&load, 3)
        let line = String(format: "PERF machine load_1m=%.1f load_5m=%.1f cores=%d", load[0], load[1], ProcessInfo.processInfo.activeProcessorCount)
        print(line)
        lines.append(line)
    }

    private static func ms(_ value: Double?) -> String {
        value.map { String(format: "%.1f", $0) } ?? "timeout"
    }

    private func waitFor(_ what: String, timeout: Double = 30, _ done: () -> Bool) async throws {
        let deadline = Date().addingTimeInterval(timeout)
        while !done() {
            if Date() >= deadline {
                XCTFail("timed out waiting for \(what)")
                return
            }
            try await Task.sleep(for: .milliseconds(1))
        }
    }

    /// Waits until no view model has been fetched for `quiet` seconds; the
    /// time of the last one, from `since`.
    private func settle(_ core: Core, since: ContinuousClock.Instant, quiet: Double = 1.5, limit: Double = 90) async throws -> Double {
        var count = core.stats.refreshes
        var lastChange = ContinuousClock.now
        while (ContinuousClock.now - lastChange).seconds < quiet, (ContinuousClock.now - since).seconds < limit {
            try await Task.sleep(for: .milliseconds(20))
            if core.stats.refreshes != count {
                count = core.stats.refreshes
                lastChange = .now
            }
        }
        return (lastChange - since).seconds
    }

    /// The app's own window content for core window `id`: sidebar, path
    /// bar, folder, status bar, as a WindowGroup window shows it.
    private func show(_ core: Core, window id: UInt32) {
        let window = NSWindow(
            contentRect: NSRect(x: 0, y: 0, width: 1200, height: 800),
            styleMask: [.titled, .resizable, .miniaturizable],
            backing: .buffered,
            defer: false
        )
        window.contentView = NSHostingView(rootView: WindowContent(core: core, window: .constant(id))
            .frame(minWidth: WindowContent.minSize.width, minHeight: WindowContent.minSize.height))
        // Off screen: drawn and laid out, never in the user's way.
        window.setFrameOrigin(NSPoint(x: -20000, y: -20000))
        window.orderFrontRegardless()
        self.window = window
    }

    /// The scroll view of the folder's rows: the list's or the grid's.
    private func scrollView() -> NSScrollView? {
        func find(_ view: NSView) -> NSScrollView? {
            if let scroll = view as? NSScrollView,
               scroll.documentView is FileTableView || scroll.documentView is FileGridView { return scroll }
            for child in view.subviews {
                if let found = find(child) { return found }
            }
            return nil
        }
        return window?.contentView.flatMap(find)
    }

    /// Scrolls to `y` points down the content, as a user would: within the
    /// insets the toolbar and the sidebar leave, so a table under the
    /// sidebar isn't scrolled sideways.
    private func move(_ scroll: NSScrollView, to y: CGFloat) {
        let insets = scroll.contentInsets
        let flipped = scroll.documentView?.isFlipped ?? true
        let top = flipped ? y - insets.top : travel(scroll) - y - insets.bottom
        scroll.contentView.scroll(to: NSPoint(x: -insets.left, y: top))
        scroll.reflectScrolledClipView(scroll.contentView)
    }

    /// How far `scroll` scrolls from top to bottom.
    private func travel(_ scroll: NSScrollView) -> CGFloat {
        let insets = scroll.contentInsets
        let shown = scroll.contentView.bounds.height - insets.top - insets.bottom
        return max(0, (scroll.documentView?.frame.height ?? 0) - shown)
    }

    /// Thumbnails shown among the rows the core sent that should have one.
    private func thumbnails(_ tab: TabViewModel?) -> String {
        let rows = tab?.rows ?? []
        let media = rows.filter { name in
            ["png", "jpg", "heic", "mov", "mp4"].contains((name.name as NSString).pathExtension.lowercased())
        }
        return "\(media.filter { !$0.thumbnail.isEmpty }.count)/\(media.count)"
    }

    private func report(_ name: String, _ core: Core, from before: BridgeStats, extra: [(String, String)]) {
        let stats = core.stats
        let refreshes = stats.refreshes - before.refreshes
        let ms = { (seconds: Double) in String(format: "%.1f", seconds * 1000) }
        var fields: [(String, String)] = [
            ("refreshes", "\(refreshes)"),
            ("unchanged", "\(stats.unchanged - before.unchanged)"),
            ("renders", "\(stats.renders - before.renders)"),
            ("batches", "\(stats.batches - before.batches)"),
            ("updates", "\(stats.updates - before.updates)"),
            ("view_ms", ms(stats.viewSeconds - before.viewSeconds)),
            ("decode_ms", ms(stats.decodeSeconds - before.decodeSeconds)),
            ("update_ms", ms(stats.updateSeconds - before.updateSeconds)),
            ("kb_per_refresh", refreshes > 0 ? "\((stats.bytes - before.bytes) / refreshes / 1024)" : "0"),
            ("longest_stall_ms", ms(heartbeat.longest.seconds)),
            ("stalls_over_50ms", "\(heartbeat.stalls)"),
        ]
        fields += extra
        let line = "PERF \(name) " + fields.map { "\($0.0)=\($0.1)" }.joined(separator: " ")
        print(line)
        lines.append(line)
    }

    private func open(_ folder: String, display: DisplayType) async throws -> (Core, UInt32, ContinuousClock.Instant) {
        try? FileManager.default.createDirectory(at: Self.stateDir, withIntermediateDirectories: true)
        try? FileManager.default.removeItem(at: Self.stateDir.appendingPathComponent("state.json"))
        let started = ContinuousClock.now
        let core = Core(picker: nil, stateFile: Self.stateDir.appendingPathComponent("state.json").path)
        var window: UInt32?
        core.start(open: folder, home: folder) { window = $0 }
        try await waitFor("a window") { core.window(window) != nil }
        let tab = try XCTUnwrap(core.window(window)?.active.id)
        core.update(.setDisplay(tab: tab, display: display))
        show(core, window: try XCTUnwrap(window))
        return (core, tab, started)
    }

    /// The window as drawn, to `DIRFLEET_PERF_SNAPSHOT`-<name>.png, for
    /// looking at what was measured.
    private func snapshot(_ name: String) {
        guard let base = ProcessInfo.processInfo.environment["DIRFLEET_PERF_SNAPSHOT"],
              let view = window?.contentView,
              let bitmap = view.bitmapImageRepForCachingDisplay(in: view.bounds) else { return }
        view.cacheDisplay(in: view.bounds, to: bitmap)
        try? bitmap.representation(using: .png, properties: [:])?.write(to: URL(fileURLWithPath: "\(base)-\(name).png"))
    }

    /// Back to the top, then down again at an ordinary pace.
    private func scrollMoreGently(_ name: String, _ core: Core, tab: UInt32) async throws {
        if let scroll = scrollView() {
            move(scroll, to: 0)
            _ = try await settle(core, since: .now, quiet: 0.5)
        }
        try await self.scroll("scroll_\(name)_moderate", core, tab: tab, step: 60)
    }

    /// 30 rows the core has sent, to click.
    private func clickable(_ core: Core, tab: UInt32) throws -> [UInt32] {
        let rows = try XCTUnwrap(core.tab(tab)?.rows.map(\.index))
        XCTAssertGreaterThanOrEqual(rows.count, 30)
        return Array(rows.prefix(30))
    }

    /// Scrolls to the end `step` points a frame: 240 is a hard flick, 60 an
    /// ordinary quick scroll.
    private func scroll(_ name: String, _ core: Core, tab: UInt32, step: CGFloat = 240, steps: Int? = nil) async throws {
        guard let scroll = scrollView(), let document = scroll.documentView else {
            XCTFail("no scroll view for \(name)")
            return
        }
        let before = core.stats
        heartbeat.start()
        let start = ContinuousClock.now
        var y: CGFloat = 0
        // A huge folder is scrolled for `steps` frames, not to its end.
        let bottom = min(travel(scroll), steps.map { CGFloat($0) * step } ?? .infinity)
        var blank = 0
        while y < bottom {
            y = min(bottom, y + step)
            move(scroll, to: y)
            try await Task.sleep(for: .milliseconds(16))
            // Rows in sight the view model doesn't hold yet show blank.
            if let (shown, first, rows) = core.stats.viewport, shown == tab, let current = core.tab(tab) {
                let held = current.firstRow ..< current.firstRow + UInt32(current.rows.count)
                if !held.contains(first) || !held.contains(first + rows - 1) { blank += 1 }
            }
        }
        let scrolled = (ContinuousClock.now - start).seconds
        let settled = try await settle(core, since: start)
        heartbeat.stop()
        report(name, core, from: before, extra: [
            ("steps_with_blank_rows", "\(blank)"),
            ("scroll_s", String(format: "%.2f", scrolled)),
            ("settled_s", String(format: "%.2f", settled)),
            ("height", "\(Int(document.frame.height))"),
            ("thumbnails", thumbnails(core.tab(tab))),
        ])
    }

    /// Clicks for a minute, for attaching a profiler (`DIRFLEET_PERF_ONLY=clicks`).
    func testClicksForProfiling() async throws {
        let folder = try fixture()
        guard ProcessInfo.processInfo.environment["DIRFLEET_PERF_ONLY"] == "clicks" else {
            throw XCTSkip("only with DIRFLEET_PERF_ONLY=clicks")
        }
        let (core, tab, started) = try await open(folder, display: .icons)
        try await waitFor("the listing") { (core.tab(tab)?.totalRows ?? 0) > 800 }
        _ = try await settle(core, since: started)
        print("PERF profiling_clicks_start")
        let end = ContinuousClock.now + .seconds(60)
        var row: UInt32 = 0
        while ContinuousClock.now < end {
            core.update(.click(tab: tab, row: row % 30, command: false, shift: false))
            row += 1
            try await Task.sleep(for: .milliseconds(150))
        }
    }

    /// Scrolls the grid up and down for a minute, for a profiler
    /// (`DIRFLEET_PERF_ONLY=scroll`, or `scroll-list` for the list).
    func testScrollingForProfiling() async throws {
        let folder = try fixture()
        let only = ProcessInfo.processInfo.environment["DIRFLEET_PERF_ONLY"] ?? ""
        guard only == "scroll" || only == "scroll-list" else { throw XCTSkip("only with DIRFLEET_PERF_ONLY=scroll") }
        let (core, tab, started) = try await open(folder, display: only == "scroll-list" ? .list : .icons)
        try await waitFor("the listing") { (core.tab(tab)?.totalRows ?? 0) > 800 }
        _ = try await settle(core, since: started)
        let scroll = try XCTUnwrap(scrollView())
        let bottom = travel(scroll)
        print("PERF profiling_scroll_start")
        let end = ContinuousClock.now + .seconds(60)
        var y: CGFloat = 0
        var step: CGFloat = 240
        while ContinuousClock.now < end {
            y += step
            if y >= bottom || y <= 0 { step = -step; y = min(max(y, 0), bottom) }
            move(scroll, to: y)
            try await Task.sleep(for: .milliseconds(16))
        }
    }

    /// Goes to another folder and back for a minute, for attaching a
    /// profiler (`DIRFLEET_PERF_ONLY=navigate`).
    func testNavigationForProfiling() async throws {
        let folder = try fixture()
        guard ProcessInfo.processInfo.environment["DIRFLEET_PERF_ONLY"] == "navigate" else {
            throw XCTSkip("only with DIRFLEET_PERF_ONLY=navigate")
        }
        let (core, tab, started) = try await open(folder, display: .icons)
        try await waitFor("the listing") { (core.tab(tab)?.totalRows ?? 0) > 800 }
        _ = try await settle(core, since: started)
        print("PERF profiling_navigation_start")
        let end = ContinuousClock.now + .seconds(60)
        while ContinuousClock.now < end {
            core.update(.navigate(tab: tab, path: (folder as NSString).appendingPathComponent(".originals")))
            try await waitFor("the other folder") { (core.tab(tab)?.totalRows ?? 999) < 50 }
            try await Task.sleep(for: .milliseconds(100))
            core.update(.navigate(tab: tab, path: folder))
            try await waitFor("the folder again") { (core.tab(tab)?.totalRows ?? 0) > 800 }
            try await Task.sleep(for: .milliseconds(100))
        }
    }

    /// Only the scrolling scenarios, warm (`DIRFLEET_PERF_ONLY=scrollbench`).
    func testScrollBench() async throws {
        let folder = try fixture()
        guard ProcessInfo.processInfo.environment["DIRFLEET_PERF_ONLY"] == "scrollbench" else {
            throw XCTSkip("only with DIRFLEET_PERF_ONLY=scrollbench")
        }
        let (core, tab, started) = try await open(folder, display: .icons)
        try await waitFor("the listing") { (core.tab(tab)?.totalRows ?? 0) > 800 }
        _ = try await settle(core, since: started)
        snapshot("icons")
        try await scroll("scroll_icons", core, tab: tab)
        try await scrollMoreGently("icons", core, tab: tab)
        snapshot("icons-end")
        core.update(.setDisplay(tab: tab, display: .list))
        _ = try await settle(core, since: .now, quiet: 0.5)
        try await scroll("scroll_list", core, tab: tab)
        try await scrollMoreGently("list", core, tab: tab)
        snapshot("list-end")
    }

    func testTheDesktopLikeFolder() async throws {
        let folder = try fixture()
        let only = ProcessInfo.processInfo.environment["DIRFLEET_PERF_ONLY"] ?? ""
        guard only.isEmpty || only == "desktop" else { throw XCTSkip("another scenario was asked for") }
        reportLoad()

        // Cold: no thumbnails cached yet.
        try? FileManager.default.removeItem(at: Self.cacheDir.appendingPathComponent("thumbnails"))
        try? FileManager.default.createDirectory(at: Self.cacheDir.appendingPathComponent("thumbnails"), withIntermediateDirectories: true)
        heartbeat.start()
        var (core, tab, started) = try await open(folder, display: .icons)
        var before = BridgeStats()
        try await waitFor("the listing") { (core.tab(tab)?.totalRows ?? 0) > 800 }
        var listed = (ContinuousClock.now - started).seconds
        var settled = try await settle(core, since: started)
        heartbeat.stop()
        report("open_icons_cold", core, from: before, extra: [
            ("listed_s", String(format: "%.2f", listed)),
            ("settled_s", String(format: "%.2f", settled)),
            ("thumbnails", thumbnails(core.tab(tab))),
        ])

        // Warm: the same folder again in a new core.
        window?.orderOut(nil)
        heartbeat.start()
        (core, tab, started) = try await open(folder, display: .icons)
        try await waitFor("the listing") { (core.tab(tab)?.totalRows ?? 0) > 800 }
        listed = (ContinuousClock.now - started).seconds
        settled = try await settle(core, since: started)
        heartbeat.stop()
        report("open_icons_warm", core, from: before, extra: [
            ("listed_s", String(format: "%.2f", listed)),
            ("settled_s", String(format: "%.2f", settled)),
            ("thumbnails", thumbnails(core.tab(tab))),
        ])

        // Clicks: how long until the view model has the selection (p50, p95)
        // and until it is drawn, and what each costs.
        before = core.stats
        heartbeat.start()
        var latencies: [Double] = []
        var drawn: [Double] = []
        for row in try clickable(core, tab: tab) {
            // As a click on a tile: sent, and the view fetched at once.
            let times = try await time {
                core.click(tab: tab, row: row, command: false, shift: false)
            } until: { core.tab(tab)?.rows.first { $0.index == row }?.selected == true }
            let clicked = try XCTUnwrap(times, "click \(row)")
            latencies.append(clicked.model)
            drawn.append(clicked.drawn)
            try await Task.sleep(for: .milliseconds(150))
        }
        heartbeat.stop()
        latencies.sort()
        drawn.sort()
        report("click_icons", core, from: before, extra: [
            ("clicks", "30"),
            ("p50_ms", Self.ms(latencies[15])),
            ("p95_ms", Self.ms(latencies[28])),
            ("drawn_p50_ms", Self.ms(drawn[15])),
            ("drawn_p95_ms", Self.ms(drawn[28])),
        ])

        // Going to a folder and back: listing it again, warm.
        before = core.stats
        heartbeat.start()
        var trips: [Double] = []
        for _ in 0..<5 {
            core.update(.navigate(tab: tab, path: (folder as NSString).appendingPathComponent(".originals")))
            try await waitFor("the other folder") { (core.tab(tab)?.totalRows ?? 999) < 50 }
            let start = ContinuousClock.now
            core.update(.navigate(tab: tab, path: folder))
            try await waitFor("the folder again") { (core.tab(tab)?.totalRows ?? 0) > 800 }
            trips.append((ContinuousClock.now - start).seconds)
        }
        heartbeat.stop()
        trips.sort()
        report("navigate_back", core, from: before, extra: [
            ("p50_ms", String(format: "%.1f", trips[2] * 1000)),
            ("max_ms", String(format: "%.1f", trips[4] * 1000)),
        ])
        _ = try await settle(core, since: .now, quiet: 0.5)

        // Idle: nothing should need drawing, and little should run. (Cores
        // of the scenarios before this one may still have threads.)
        before = core.stats
        let resting = processStats()
        heartbeat.start()
        try await Task.sleep(for: .seconds(15))
        heartbeat.stop()
        let rested = processStats()
        report("idle_15s", core, from: before, extra: [
            ("cpu_ms", Self.ms((rested.cpu - resting.cpu) * 1000)),
            ("threads", "\(rested.threads)"),
            ("footprint_mb", String(format: "%.0f", rested.footprint)),
        ])

        try await scroll("scroll_icons", core, tab: tab)
        try await scrollMoreGently("icons", core, tab: tab)

        core.update(.setDisplay(tab: tab, display: .list))
        _ = try await settle(core, since: .now, quiet: 0.5)
        try await scroll("scroll_list", core, tab: tab)
        try await scrollMoreGently("list", core, tab: tab)

        // Ten windows on the folder: every refresh carries all of them. Back
        // at the top first, so the rows clicked are the ones the core sends.
        if let scroll = scrollView() {
            move(scroll, to: 0)
            _ = try await settle(core, since: .now, quiet: 0.5)
        }
        for _ in 0..<9 {
            core.update(.newWindow(path: folder))
        }
        try await waitFor("ten windows") {
            core.view.snapshot.count == 10 && core.view.snapshot.allSatisfy { $0.active.totalRows > 800 }
        }
        _ = try await settle(core, since: .now, quiet: 1)
        before = core.stats
        heartbeat.start()
        latencies = []
        drawn = []
        for row in try clickable(core, tab: tab) {
            let times = try await time {
                core.update(.click(tab: tab, row: row, command: false, shift: false))
            } until: { core.tab(tab)?.rows.first { $0.index == row }?.selected == true }
            let clicked = try XCTUnwrap(times, "click \(row)")
            latencies.append(clicked.model)
            drawn.append(clicked.drawn)
            try await Task.sleep(for: .milliseconds(100))
        }
        heartbeat.stop()
        latencies.sort()
        drawn.sort()
        report("click_list_10_windows", core, from: before, extra: [
            ("p50_ms", Self.ms(latencies[15])),
            ("p95_ms", Self.ms(latencies[28])),
            ("drawn_p50_ms", Self.ms(drawn[15])),
            ("drawn_p95_ms", Self.ms(drawn[28])),
        ])
        writeLines()
    }

    /// The lines so far, added to `DIRFLEET_PERF_OUT` when that is set.
    private func writeLines() {
        guard let out = ProcessInfo.processInfo.environment["DIRFLEET_PERF_OUT"], !out.isEmpty else { return }
        let text = lines.joined(separator: "\n") + "\n"
        if let file = FileHandle(forWritingAtPath: out) {
            file.seekToEndOfFile()
            file.write(Data(text.utf8))
            try? file.close()
        } else {
            try? text.write(toFile: out, atomically: true, encoding: .utf8)
        }
        lines = []
    }

    /// Launching with a saved window: how long the core takes to start on
    /// the main thread, and until the restored folder's rows are drawn.
    func testLaunchingWithSavedWindows() async throws {
        let folder = try fixture()
        let only = ProcessInfo.processInfo.environment["DIRFLEET_PERF_ONLY"] ?? ""
        guard only.isEmpty || only == "launch" else { throw XCTSkip("another scenario was asked for") }
        reportLoad()
        // A first run leaves the state a launch restores.
        let state = Self.stateDir.appendingPathComponent("state.json")
        do {
            let (core, tab, started) = try await open(folder, display: .icons)
            try await waitFor("the listing") { (core.tab(tab)?.totalRows ?? 0) > 800 }
            _ = try await settle(core, since: started)
            try await waitFor("the saved state") { FileManager.default.fileExists(atPath: state.path) }
            window?.orderOut(nil)
        }
        for run in 1...3 {
            heartbeat.start()
            let start = ContinuousClock.now
            let core = Core(picker: nil, stateFile: state.path)
            let made = (ContinuousClock.now - start).seconds * 1000
            var id: UInt32?
            core.start(open: nil, home: folder) { id = $0 }
            try await waitFor("a window") { core.window(id) != nil }
            let restored = (ContinuousClock.now - start).seconds * 1000
            show(core, window: try XCTUnwrap(id))
            try await waitFor("the rows") { !(core.window(id)?.active.rows.isEmpty ?? true) }
            draw()
            let rows = (ContinuousClock.now - start).seconds * 1000
            heartbeat.stop()
            report("launch_\(run)", core, from: BridgeStats(), extra: [
                ("core_init_ms", Self.ms(made)),
                ("window_ms", Self.ms(restored)),
                ("first_rows_drawn_ms", Self.ms(rows)),
            ])
            _ = try await settle(core, since: .now, quiet: 0.5)
            window?.orderOut(nil)
        }
        writeLines()
    }

    /// Flat folders of 10,000 and 100,000 files, as a list and as icons:
    /// opening, selecting everything, clicking, reversing the order, jumping
    /// and flicking through, and coming Back with everything selected.
    func testLargeFolders() async throws {
        let only = ProcessInfo.processInfo.environment["DIRFLEET_PERF_ONLY"] ?? ""
        guard only.isEmpty || only.hasPrefix("large") else { throw XCTSkip("another scenario was asked for") }
        let small = try flatFixture(10)
        reportLoad()
        for count in only == "large-10k" ? [10000] : [10000, 100_000] {
            let folder = try flatFixture(count)
            let total = UInt32(count)
            for display in [DisplayType.list, .icons] {
                let name = "large_\(count / 1000)k_\(display == .list ? "list" : "icons")"
                heartbeat.start()
                let before = BridgeStats()
                let (core, tab, started) = try await open(folder, display: display)
                let shown = { core.tab(tab) }
                try await waitFor("the listing", timeout: 120) { shown()?.totalRows == total }
                let listed = (ContinuousClock.now - started).seconds * 1000
                draw()
                let opened = (ContinuousClock.now - started).seconds * 1000
                _ = try await settle(core, since: .now, quiet: 0.5)
                let all = try await time { core.update(.selectAll(tab: tab)) } until: { shown()?.selectedCount == total }
                // Each event and each view pays for the selection now.
                let clickSelected = try await time {
                    core.update(.click(tab: tab, row: 3, command: true, shift: false))
                } until: { shown()?.selectedCount == total - 1 }
                let click = try await time {
                    core.click(tab: tab, row: 5, command: false, shift: false)
                } until: { shown()?.selectedCount == 1 }
                // Reversed, the first row is another; then back.
                let top = shown()?.rows.first?.name
                let inverted = shown()?.view.invert ?? false
                let reverse = try await time { core.update(.setReverse(tab: tab, reverse: !inverted)) } until: {
                    shown()?.rows.first?.name != top
                }
                _ = try await time { core.update(.setReverse(tab: tab, reverse: inverted)) } until: {
                    shown()?.rows.first?.name == top
                }
                // Halfway down in one move, as dragging the scroller does.
                var jump: (model: Double, drawn: Double)?
                if let scroll = scrollView() {
                    let middle = total / 2
                    jump = try await time { move(scroll, to: travel(scroll) / 2) } until: {
                        // The rows around the middle have come.
                        guard let tab = shown(), !tab.rows.isEmpty else { return false }
                        return tab.firstRow > total / 4 && tab.firstRow < middle + 2000
                    }
                    move(scroll, to: 0)
                    _ = try await settle(core, since: .now, quiet: 0.5)
                }
                heartbeat.stop()
                report(name, core, from: before, extra: [
                    ("listed_ms", Self.ms(listed)),
                    ("open_drawn_ms", Self.ms(opened)),
                    ("select_all_ms", Self.ms(all?.drawn)),
                    ("cmd_click_all_selected_ms", Self.ms(clickSelected?.drawn)),
                    ("click_ms", Self.ms(click?.drawn)),
                    ("reverse_ms", Self.ms(reverse?.drawn)),
                    ("jump_ms", Self.ms(jump?.drawn)),
                ])

                try await scroll("\(name)_flick", core, tab: tab, steps: 150)

                // Back into the folder with everything selected, which is
                // selected again by name.
                if (ProcessInfo.processInfo.environment["DIRFLEET_PERF_SKIP_BACK"] ?? "").isEmpty {
                    let stats = core.stats
                    heartbeat.start()
                    _ = try await time { core.update(.selectAll(tab: tab)) } until: { shown()?.selectedCount == total }
                    _ = try await time { core.update(.navigate(tab: tab, path: small)) } until: { shown()?.totalRows == 10 }
                    let back = try await time(timeout: 600) { core.update(.back(tab: tab)) } until: {
                        shown()?.totalRows == total && shown()?.selectedCount == total
                    }
                    heartbeat.stop()
                    report("\(name)_back_selected", core, from: stats, extra: [("back_ms", Self.ms(back?.drawn))])
                }
                window?.orderOut(nil)
                writeLines()
            }
        }
    }
}
