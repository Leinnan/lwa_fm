#!/bin/sh
# Builds a folder shaped like a busy macOS Desktop, for the Mac app's
# performance tests: 723 screenshots (PNG), 69 MOV and 22 MP4 screen
# recordings (H.264 and HEVC), JPEGs, HEIC photos, app bundles and a few
# documents, 847 entries in all, as measured on a real Desktop on
# 2026-11-09. Content is generated (FFmpeg test patterns), never copied.
# Files are APFS clones of a few originals, so the folder costs little disk;
# each still has its own path and is thumbnailed on its own.
#
# Usage: scripts/desktop_like_fixture.sh [out-dir]
# Default: $TMPDIR/dirfleet_perf_desktop. Needs ffmpeg and sips.
set -eu

out=${1:-${TMPDIR:-/tmp}/dirfleet_perf_desktop}
if [ -f "$out/.complete" ]; then
    echo "$out is ready"
    exit 0
fi
command -v ffmpeg >/dev/null || { echo "needs ffmpeg" >&2; exit 1; }
rm -rf "$out"
mkdir -p "$out/.originals"
orig="$out/.originals"

# Screenshot sizes seen on the Desktop, with patterns that compress like
# screenshots (flat colour and text) and like photos (busy).
i=0
for spec in 2784x1728:testsrc2 3456x2234:testsrc2 1766x906:smptehdbars \
    2696x1640:mandelbrot 1824x1240:testsrc2 3144x2038:rgbtestsrc; do
    size=${spec%%:*}
    source=${spec#*:}
    ffmpeg -loglevel error -f lavfi -i "$source=size=$size" -frames:v 1 "$orig/shot$i.png"
    i=$((i + 1))
done
ffmpeg -loglevel error -f lavfi -i "mandelbrot=size=4032x3024" -frames:v 1 -q:v 3 "$orig/photo.jpg"
sips -s format heic "$orig/photo.jpg" --out "$orig/photo.heic" >/dev/null

# Screen recordings: H.264 and HEVC, a few seconds each.
ffmpeg -loglevel error -f lavfi -i "testsrc2=size=2784x1728:rate=30" -t 8 \
    -c:v libx264 -pix_fmt yuv420p "$orig/rec_h264.mov"
ffmpeg -loglevel error -f lavfi -i "testsrc2=size=1886x1440:rate=30" -t 10 \
    -c:v libx265 -tag:v hvc1 -pix_fmt yuv420p "$orig/rec_hevc.mov"
ffmpeg -loglevel error -f lavfi -i "testsrc2=size=2300x1488:rate=30" -t 4 \
    -c:v libx264 -pix_fmt yuv420p "$orig/rec.mp4"

clone() { cp -c "$1" "$2" 2>/dev/null || cp "$1" "$2"; }

n=1
while [ $n -le 723 ]; do
    clone "$orig/shot$((n % 6)).png" "$out/Zrzut ekranu $(printf %04d $n).png"
    n=$((n + 1))
done
n=1
while [ $n -le 69 ]; do
    if [ $((n % 3)) -eq 0 ]; then src=rec_hevc.mov; else src=rec_h264.mov; fi
    clone "$orig/$src" "$out/Nagranie z ekranu $(printf %03d $n).mov"
    n=$((n + 1))
done
n=1
while [ $n -le 22 ]; do
    clone "$orig/rec.mp4" "$out/clip $(printf %02d $n).mp4"
    n=$((n + 1))
done
for n in 1 2 3 4 5 6 7; do clone "$orig/photo.jpg" "$out/IMG_09$n.jpg"; done
for n in 1 2; do clone "$orig/photo.heic" "$out/IMG_016$n.HEIC"; done
for name in Alpha Beta Gamma Delta Epsilon; do
    mkdir -p "$out/$name.app/Contents/MacOS"
    printf '<?xml version="1.0"?><plist version="1.0"><dict><key>CFBundleName</key><string>%s</string></dict></plist>' \
        "$name" >"$out/$name.app/Contents/Info.plist"
done
for n in 1 2 3; do printf '%%PDF-1.1\n%%%%EOF\n' >"$out/document $n.pdf"; done
for n in 1 2; do echo "notes $n" >"$out/notes $n.txt"; done
for n in 1 2; do echo "From: x" >"$out/mail $n.eml"; done
for n in 1 2 3; do echo "[Desktop Entry]" >"$out/game $n.desktop"; done
for n in 1 2; do echo "[InternetShortcut]" >"$out/link $n.url"; done
for n in 1 2; do echo "gimp xcf file" >"$out/drawing $n.xcf"; done
mkdir -p "$out/.spacedrive"
touch "$out/.localized" "$out/.DS_Store"

touch "$out/.complete"
echo "$out: $(ls -A "$out" | grep -vc '^\.originals$') entries"
