set windows-shell := ["powershell.exe", "-NoLogo", "-NoProfile", "-NonInteractive", "-Command"]

format_bundle := if os() == "windows" { "--format wxsmsi" } else { if os() == "linux" { "--format appimage" } else { "--format osx" } }
bundle_dir := "target/release/bundle"
installer_dir := "target/release/installer"

# list available commands
list:
	just --list

# Install tools required for bundling
[group('setup')]
install-tools:
    cargo install --git https://github.com/Leinnan/cargo-bundler --rev fd6436deb575361d7d2b467dcffd037bc9e3c936

# Bundles project
[group('build')]
bundle:
    cargo bundler -r {{format_bundle}}

# Old bundles would otherwise be picked up too, the AppImage name includes the version
[unix]
_clean_bundle:
    rm -rf '{{bundle_dir}}' '{{installer_dir}}'

[windows]
_clean_bundle:
    if (Test-Path -LiteralPath '{{bundle_dir}}') { Remove-Item -LiteralPath '{{bundle_dir}}' -Recurse -Force -ErrorAction Stop }
    if (Test-Path -LiteralPath '{{installer_dir}}') { Remove-Item -LiteralPath '{{installer_dir}}' -Recurse -Force -ErrorAction Stop }

# Build the installer and copy only it to target/release/installer
[group('build')]
[windows]
installer: _clean_bundle bundle
    New-Item -ItemType Directory -Path '{{installer_dir}}' -Force -ErrorAction Stop | Out-Null
    Copy-Item -Path '{{bundle_dir}}/wsxmsi/*.msi' -Destination '{{installer_dir}}/' -ErrorAction Stop

# Build the installer and copy only it to target/release/installer
[group('build')]
[linux]
installer: _clean_bundle bundle
    mkdir -p {{installer_dir}}
    cp {{bundle_dir}}/appimage/*.AppImage {{installer_dir}}/

# Build the installer and copy only it to target/release/installer
[group('build')]
[macos]
installer: _clean_bundle create_mac_installer
    mkdir -p {{installer_dir}}
    cp DirFleetInstaller.pkg {{installer_dir}}/

# Clippy fixes and formatting
[group('code')]
fix:
	cargo clippy --fix --allow-staged --allow-dirty
	cargo fmt --all

# Validates project
[group('code')]
validate:
    cargo fmt --all -- --check
    cargo test
    cargo clippy -- -D warnings

_gen_icon source size postfix:
    rsvg-convert -w {{size}} -h {{size}} {{source}} -o static/icon.iconset/icon_{{postfix}}.png

# Build the window icon and Windows icon from the SVGs, without the tile's margin
[group('setup')]
[unix]
make_win_icon:
    rsvg-convert static/icon.svg | magick - -trim +repage icon_full.png
    rsvg-convert static/icon_small.svg | magick - -trim +repage icon_small.png
    magick icon_full.png -resize 256x256 static/window_icon.png
    magick \( icon_small.png -resize 16x16 \) \( icon_small.png -resize 24x24 \) \( icon_small.png -resize 32x32 \) \( icon_full.png -resize 48x48 \) \( icon_full.png -resize 64x64 \) \( icon_full.png -resize 128x128 \) \( icon_full.png -resize 256x256 \) static/icon.ico
    rm icon_full.png icon_small.png

# Windows PowerShell cannot pipe binary PNG data without corrupting it.
[group('setup')]
[windows]
make_win_icon:
    rsvg-convert static/icon.svg -o icon_full.png
    rsvg-convert static/icon_small.svg -o icon_small.png
    magick icon_full.png -trim +repage icon_full.png
    magick icon_small.png -trim +repage icon_small.png
    magick icon_full.png -resize 256x256 static/window_icon.png
    magick '(' icon_small.png -resize 16x16 ')' '(' icon_small.png -resize 24x24 ')' '(' icon_small.png -resize 32x32 ')' '(' icon_full.png -resize 48x48 ')' '(' icon_full.png -resize 64x64 ')' '(' icon_full.png -resize 128x128 ')' '(' icon_full.png -resize 256x256 ')' static/icon.ico
    Remove-Item -LiteralPath icon_full.png, icon_small.png -ErrorAction Stop

# Build the icon for macOS app
[group('setup')]
[macos]
make_mac_icon:
    rm -rf static/icon.iconset
    mkdir -p static/icon.iconset
    rm -rf static/resources
    mkdir -p static/resources
    just _gen_icon static/icon_small.svg 16 "16x16"
    just _gen_icon static/icon_small.svg 32 "16x16@2x"
    just _gen_icon static/icon_small.svg 32 "32x32"
    just _gen_icon static/icon.svg 64 "32x32@2x"
    just _gen_icon static/icon.svg 128 "128x128"
    just _gen_icon static/icon.svg 256 "128x128@2x"
    just _gen_icon static/icon.svg 256 "256x256"
    just _gen_icon static/icon.svg 512 "256x256@2x"
    just _gen_icon static/icon.svg 512 "512x512"
    just _gen_icon static/icon.svg 1024 "512x512@2x"

    iconutil -c icns static/icon.iconset
    rm -rf static/icon.iconset
    mv static/icon.icns static/resources/icon.icns

# Code signing identity for the macOS app. macOS remembers folder access
# (and Full Disk Access) per signing identity, so a stable one keeps those
# grants across rebuilds. Override with DIRFLEET_SIGN_IDENTITY, e.g. a
# "Developer ID Application: ..." one for distribution, or "-" for ad-hoc.
sign_identity := env("DIRFLEET_SIGN_IDENTITY", "Apple Development: PIOTR PAWEL SIUSZKO (348SLF5L4U)")

# Build the macOS app
[group('build')]
[macos]
build_mac_app: make_mac_icon
    rm -rf DirFleet.app
    cargo build --release
    mkdir -p DirFleet.app
    mkdir -p DirFleet.app/Contents
    mkdir -p DirFleet.app/Contents/MacOS
    mkdir -p DirFleet.app/Contents/Resources
    cp static/Info.plist DirFleet.app/Contents/Info.plist
    cp static/resources/* DirFleet.app/Contents/Resources/
    cp target/release/lwa_fm DirFleet.app/Contents/MacOS/lwa_fm
    just sign_mac_app

# Sign DirFleet.app with the hardened runtime
[group('build')]
[macos]
sign_mac_app:
    codesign --force --options runtime --timestamp=none --entitlements static/DirFleet.entitlements --sign "{{sign_identity}}" DirFleet.app
    codesign --verify --strict --verbose=2 DirFleet.app

# Create the macOS installer
[macos]
[group('build')]
create_mac_installer: build_mac_app
    pkgbuild --install-location /Applications --component DirFleet.app DirFleetInstaller.pkg

apple_dir := "apple"
apple_app := apple_dir / "build/Build/Products/Release/DirFleet.app"
apple_xcodebuild := "xcodebuild -project DirFleetNative.xcodeproj -scheme DirFleet-macOS -configuration Release -derivedDataPath build ONLY_ACTIVE_ARCH=YES -skipPackagePluginValidation"
apple_xcodebuild_perf := "xcodebuild -project DirFleetNative.xcodeproj -scheme DirFleet-perf -configuration Release -derivedDataPath build ONLY_ACTIVE_ARCH=YES -skipPackagePluginValidation"
# Must match the `boltffi` crate version in crates/dirfleet_ffi/Cargo.toml.
boltffi_version := "0.29.3"
# The pinned boltffi lives here, apart from any global one other projects use.
boltffi_root := justfile_directory() / "target/tools/boltffi-" + boltffi_version

# Install the pinned boltffi into target/tools (once)
[group('native')]
[macos]
apple-tools:
    @test -x "{{boltffi_root}}/bin/boltffi" || cargo install boltffi_cli --version '={{boltffi_version}}' --locked --root "{{boltffi_root}}"

# Check the tools the native macOS shell needs
[group('native')]
[macos]
apple-doctor: apple-tools
    @command -v xcodebuild >/dev/null || { echo "xcodebuild missing: install Xcode"; exit 1; }
    @command -v xcodegen >/dev/null || { echo "xcodegen missing: brew install xcodegen"; exit 1; }
    @"{{boltffi_root}}/bin/boltffi" --version | grep -q " {{boltffi_version}}$" || { echo "{{boltffi_root}} doesn't hold boltffi {{boltffi_version}}: remove it and run just apple-tools"; exit 1; }
    @rustup target list --installed | grep -q aarch64-apple-darwin || { echo "missing Rust target: rustup target add aarch64-apple-darwin"; exit 1; }
    @echo "native shell tools OK"

# Swift types for the core's events, effects and view model
[group('native')]
[macos]
apple-typegen:
    cargo run -p dirfleet_ffi --bin codegen --features codegen -- --output-dir {{apple_dir}}/generated
    # The generated reader makes an array per number read; ours reads in place.
    cp {{apple_dir}}/Serde/BinaryDeserializer.swift {{apple_dir}}/generated/App/Sources/Serde/BinaryDeserializer.swift

# The Rust core as a Swift package
[group('native')]
[macos]
[working-directory('crates/dirfleet_ffi')]
apple-package: apple-doctor
    rm -rf ../../{{apple_dir}}/generated/Shared
    PATH="{{boltffi_root}}/bin:$PATH" boltffi pack apple --release
    # lua-src links Lua as `static:-bundle`, which leaves it out of the static
    # library; the Swift package gets it merged in (the newest build's copy).
    lua="$(ls -t ../../target/aarch64-apple-darwin/release/build/mlua-sys-*/out/lib/liblua5.4.a | head -1)" && lib=../../{{apple_dir}}/generated/Shared/Shared.xcframework/macos-arm64/libdirfleet_ffi.a && libtool -static -o "$lib.merged" "$lib" "$lua" && mv "$lib.merged" "$lib"

# Generate everything the native macOS shell's Xcode project needs
[group('native')]
[macos]
apple-generate: apple-typegen apple-package
    cd {{apple_dir}} && xcodegen

# Build the native macOS shell
[group('native')]
[macos]
apple-build: apple-generate
    cd {{apple_dir}} && {{apple_xcodebuild}} build

# Build and launch the native macOS shell
[group('native')]
[macos]
apple-run: apple-build
    open {{apple_app}}

# Test the native macOS shell against the shared golden view models
[group('native')]
[macos]
apple-test: apple-generate
    cd {{apple_dir}} && {{apple_xcodebuild}} test -only-testing:DirFleetTests

# Measure the native macOS shell on a folder shaped like a busy Desktop and on 10k and 100k files (Release); PERF lines show the numbers
[group('native')]
[macos]
apple-perf out="": apple-generate
    scripts/desktop_like_fixture.sh
    cd {{apple_dir}} && TEST_RUNNER_DIRFLEET_PERF=1 TEST_RUNNER_DIRFLEET_PERF_OUT="{{out}}" TEST_RUNNER_DIRFLEET_PERF_ONLY="${DIRFLEET_PERF_ONLY:-}" TEST_RUNNER_DIRFLEET_PERF_SKIP_BACK="${DIRFLEET_PERF_SKIP_BACK:-}" {{apple_xcodebuild_perf}} test -only-testing:DirFleetTests/PerfTests 2>&1 | grep -E "^PERF|error:|Executed"

# Drive the native macOS shell's UI on fixture folders (needs UI automation permission)
[group('native')]
[macos]
apple-uitest: apple-generate
    cd {{apple_dir}} && {{apple_xcodebuild}} test -only-testing:DirFleetUITests

# Check the parity ledger against the code and show how far the Mac and Windows apps have come
[group('native')]
parity:
    cargo test -q -p dirfleet_ffi --test parity -- --nocapture

# Generate the Xcode project and open it
[group('native')]
[macos]
apple-open: apple-generate
    xed {{apple_dir}}/DirFleetNative.xcodeproj

# Remove everything generated for the native macOS shell
[group('native')]
[macos]
apple-clean:
    rm -rf {{apple_dir}}/generated {{apple_dir}}/build {{apple_dir}}/DirFleetNative.xcodeproj

# Lint the native Windows shell (WinUI 3, `windows/`)
[group('native')]
[windows]
windows-check:
    cargo fmt -p dirfleet_windows -- --check
    cargo clippy -p dirfleet_windows --all-targets -- -D warnings

# Test the native Windows shell
[group('native')]
[windows]
windows-test:
    cargo test -p dirfleet_windows

# Measure what the native Windows shell waits on the core for in folders of 10,000 and 100,000 entries
[group('native')]
[windows]
windows-perf:
    cargo test -p dirfleet_windows --release -- --ignored --nocapture perf_of

# Build and launch the native Windows shell; arguments go to the app (`--folder C:\`, `--state-file f.json`)
[group('native')]
[windows]
windows-run *args:
    cargo run -p dirfleet_windows -- {{args}}

# Build the native Windows shell's MSI into target/windows-package/installer (needs WiX 5); `-NoSmoke` skips starting the unpacked app
[group('native')]
[windows]
windows-package *args:
    powershell.exe -NoLogo -NoProfile -NonInteractive -ExecutionPolicy Bypass -File windows/packaging/package.ps1 {{args}}
