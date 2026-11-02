# list available commands
list:
	just --list

# Install tools required for bundling
[group('setup')]
install-tools:
    cargo install cargo-bundle

# Bundles project
[group('build')]
bundle:
    cargo bundle --release

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
make_win_icon:
    rsvg-convert static/icon.svg | magick - -trim +repage icon_full.png
    rsvg-convert static/icon_small.svg | magick - -trim +repage icon_small.png
    magick icon_full.png -resize 256x256 static/window_icon.png
    magick \( icon_small.png -resize 16x16 \) \( icon_small.png -resize 24x24 \) \( icon_small.png -resize 32x32 \) \( icon_full.png -resize 48x48 \) \( icon_full.png -resize 64x64 \) \( icon_full.png -resize 128x128 \) \( icon_full.png -resize 256x256 \) static/icon.ico
    rm icon_full.png icon_small.png

# Build the icon for macOS app
[group('setup')]
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
