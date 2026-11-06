#!/bin/sh
# Render the icon sources in assets/icons/*.svg into the committed raster files:
#   AppIcon.icns       app bundle icon (scripts/bundle-macos.sh)
#   runwick-512.png    window / Dock icon embedded in the app
#   tray@2x.png        menu bar template image embedded in `runwick menubar`
# Needs Google Chrome (headless SVG rendering), sips and iconutil (macOS). Run it after editing an SVG.
set -eu

ROOT=$(cd "$(dirname "$0")/.." && pwd)
ICONS="$ROOT/assets/icons"
CHROME=${CHROME:-"/Applications/Google Chrome.app/Contents/MacOS/Google Chrome"}
[ -x "$CHROME" ] || { echo "Google Chrome not found (set CHROME=/path/to/chrome)" >&2; exit 1; }

WORK=$(mktemp -d "${TMPDIR:-/tmp}/runwick-icons.XXXXXX")
trap 'command rm -rf "$WORK"' EXIT

# render <svg> <width> <height> <out.png>: draw the SVG on a transparent page of exactly that size.
render() {
  html="$WORK/$(basename "$1" .svg).html"
  printf '<!doctype html><html><body style="margin:0;background:transparent">' >"$html"
  printf '<img src="file://%s" width="%s" height="%s" style="display:block"></body></html>' "$1" "$2" "$3" >>"$html"
  "$CHROME" --headless=new --disable-gpu --hide-scrollbars --no-first-run --no-default-browser-check \
    --user-data-dir="$WORK/profile" --allow-file-access-from-files --force-device-scale-factor=1 \
    --default-background-color=00000000 --window-size="$2,$3" --screenshot="$4" "file://$html" >/dev/null 2>&1
  [ -s "$4" ] || { echo "rendering $1 failed" >&2; exit 1; }
}

render "$ICONS/runwick.svg" 1024 1024 "$WORK/icon-1024.png"
render "$ICONS/tray.svg" 24 44 "$ICONS/tray@2x.png"

SET="$WORK/AppIcon.iconset"
mkdir -p "$SET"
for size in 16 32 128 256 512; do
  sips -z "$size" "$size" "$WORK/icon-1024.png" --out "$SET/icon_${size}x${size}.png" >/dev/null
  double=$((size * 2))
  sips -z "$double" "$double" "$WORK/icon-1024.png" --out "$SET/icon_${size}x${size}@2x.png" >/dev/null
done
iconutil -c icns "$SET" -o "$ICONS/AppIcon.icns"
cp "$SET/icon_512x512.png" "$ICONS/runwick-512.png"
echo "rendered: $ICONS/AppIcon.icns, runwick-512.png, tray@2x.png"
