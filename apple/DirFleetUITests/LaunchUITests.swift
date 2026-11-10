import XCTest

/// Launches the app on a fixture folder with its own state file, so the
/// user's windows are never read or changed.
final class LaunchUITests: XCTestCase {
    private var dir: URL!

    override func setUpWithError() throws {
        continueAfterFailure = false
        dir = FileManager.default.temporaryDirectory
            .appendingPathComponent("dirfleet_ui_\(UUID().uuidString)")
        let folder = dir.appendingPathComponent("Fixture")
        try FileManager.default.createDirectory(
            at: folder.appendingPathComponent("Inner"), withIntermediateDirectories: true
        )
        try Data("x".utf8).write(to: folder.appendingPathComponent("note.txt"))
    }

    override func tearDownWithError() throws {
        try? FileManager.default.removeItem(at: dir)
    }

    private func launch(folder: String?) -> XCUIApplication {
        let app = XCUIApplication()
        // Tests never download FFmpeg.
        app.launchEnvironment["DIRFLEET_NO_DOWNLOADS"] = "1"
        app.launchArguments = ["-stateFile", dir.appendingPathComponent("state.json").path]
        if let folder {
            app.launchArguments += ["-folder", folder]
        }
        app.launch()
        return app
    }

    func testOpensTheGivenFolderAndItsSubfolder() throws {
        let folder = dir.appendingPathComponent("Fixture")
        let app = launch(folder: folder.path)
        let window = app.windows["Fixture"]
        XCTAssertTrue(window.waitForExistence(timeout: 10), "a window shows the given folder")
        XCTAssertTrue(window.staticTexts["note.txt"].waitForExistence(timeout: 5))

        window.staticTexts["Inner"].doubleClick()
        XCTAssertTrue(app.windows["Inner"].waitForExistence(timeout: 5), "double-click opens the folder")
        app.terminate()
    }

    func testTheFolderLeftOpenComesBackOnTheNextLaunch() throws {
        let inner = dir.appendingPathComponent("Fixture/Inner")
        let first = launch(folder: inner.path)
        XCTAssertTrue(first.windows["Inner"].waitForExistence(timeout: 10))
        first.terminate()

        let second = launch(folder: nil)
        XCTAssertTrue(second.windows["Inner"].waitForExistence(timeout: 10), "the saved window is restored")
        XCTAssertEqual(second.windows.count, 1, "restored once, not also by AppKit")
        second.terminate()
    }

    /// Drawing the icon grid used to select each drawn tile in turn, so a
    /// clicked file never stayed selected.
    func testAClickedIconTileStaysSelected() throws {
        let folder = dir.appendingPathComponent("Fixture")
        for number in 1...30 {
            try Data("\(number)".utf8).write(to: folder.appendingPathComponent(String(format: "file %02d.txt", number)))
        }
        let app = launch(folder: folder.path)
        let window = app.windows["Fixture"]
        XCTAssertTrue(window.waitForExistence(timeout: 10))
        app.menuBars.menuItems["as Icons"].click()
        let tile = window.staticTexts["file 04.txt"]
        XCTAssertTrue(tile.waitForExistence(timeout: 5))
        tile.click()
        sleep(1)
        XCTAssertTrue(window.staticTexts["1 of 32 items selected"].exists, "one item stays selected")
        tile.rightClick()
        app.typeKey(.escape, modifierFlags: [])
        XCTAssertTrue(window.staticTexts["1 of 32 items selected"].exists, "a right-click keeps it")
        app.terminate()
    }
}
