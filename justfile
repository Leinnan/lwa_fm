default := "validate"

install-tools:
    cargo install cargo-bundle

bundle:
    cargo bundle --release

validate:
    cargo build
    cargo test
    cargo fmt --all -- --check
    cargo clippy -- -D warnings

_gen_icon size postfix:
    sips -z {{size}} {{size}} static/base_icon.png --out static/icon.iconset/icon_{{postfix}}.png
    cp static/icon.iconset/icon_{{postfix}}.png static/resources/{{postfix}}.png

make_win_icon:
    convert static/base_icon.png -define icon:auto-resize=16,24,32,48,64,128,256,512 static/icon.ico

# Build the icon for macOS app
make_mac_icon:
    rm -rf static/icon.iconset
    mkdir -p static/icon.iconset
    rm -rf static/resources
    mkdir -p static/resources
    just _gen_icon 16 "16x16"
    just _gen_icon 32 "32x32"
    just _gen_icon 64 "64x64"
    just _gen_icon 128 "128x128"
    just _gen_icon 256 "256x256"
    just _gen_icon 512 "512x512"
    just _gen_icon 1024 "1024x1024"
    just _gen_icon 32 "16x16@2x"
    just _gen_icon 64 "32x32@2x"
    just _gen_icon 128 "64x64@2x"
    just _gen_icon 256 "128x128@2x"
    just _gen_icon 512 "256x256@2x"
    just _gen_icon 1024 "512x512@2x"

    iconutil -c icns static/icon.iconset
    rm -rf static/icon.iconset
    mv static/icon.icns static/resources/icon.icns

# Build the macOS app
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

create_mac_installer: build_mac_app
    pkgbuild --install-location /Applications --component DirFleet.app DirFleetInstaller.pkg
