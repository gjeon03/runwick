#!/bin/sh
# Build Runwick (release) and assemble dist/Runwick.app.
#   sh scripts/bundle-macos.sh            build and bundle
#   sh scripts/bundle-macos.sh --no-build bundle the existing release binary
# The app icon comes from assets/icons/AppIcon.icns (regenerate with scripts/render-icons.sh).
# The bundle is signed ad hoc with the hardened runtime (no entitlements): the system then refuses
# DYLD_* injection and unsigned libraries, so no other program can run code as Runwick (the host
# trusts window clients by their executable). Distribution needs a Developer ID signature.
set -eu

ROOT=$(cd "$(dirname "$0")/.." && pwd)
cd "$ROOT"
TARGET_DIR=${CARGO_TARGET_DIR:-"$ROOT/target"}
APP="$ROOT/dist/Runwick.app"
BUILD=1
[ "${1:-}" = "--no-build" ] && BUILD=0

[ "$(uname -s)" = "Darwin" ] || { echo "macOS only" >&2; exit 1; }

# Version of the workspace package (Cargo.toml [workspace.package] version).
VERSION=$(sed -n '/^\[workspace.package\]/,/^\[/s/^version *= *"\([^"]*\)".*/\1/p' Cargo.toml | head -n 1)
[ -n "$VERSION" ] || { echo "could not read the version from Cargo.toml" >&2; exit 1; }

if [ "$BUILD" = 1 ]; then
  export MACOSX_DEPLOYMENT_TARGET=11.0
  cargo build --release -p runwick
fi
BIN="$TARGET_DIR/release/runwick"
[ -x "$BIN" ] || { echo "missing $BIN (build first)" >&2; exit 1; }

# Verification hooks (scripted input, snapshots, injected UI state) exist only in debug builds.
# A release binary that still names one must not ship; checked before dist/ is touched.
HOOKS="RUNWICK_VERIFY_INPUT RUNWICK_VERIFY_DIR RUNWICK_SNAPSHOT_DELAY_MS RUNWICK_UI_STATE RUNWICK_UI_OVERLAY"
FOUND=""
for hook in $HOOKS; do
  count=$(grep -a -c "$hook" "$BIN" || true)
  [ "$count" = 0 ] || FOUND="$FOUND $hook($count)"
done
[ -z "$FOUND" ] || { echo "verification hooks in $BIN:$FOUND (rebuild in release)" >&2; exit 1; }
echo "checked: no verification hooks in the release binary"

command rm -rf "$APP"
mkdir -p "$APP/Contents/MacOS" "$APP/Contents/Resources"
cp "$BIN" "$APP/Contents/MacOS/runwick"
cp "$ROOT/assets/icons/AppIcon.icns" "$APP/Contents/Resources/AppIcon.icns"
printf 'APPL????' >"$APP/Contents/PkgInfo"
cat >"$APP/Contents/Info.plist" <<EOF
<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
  <key>CFBundleDevelopmentRegion</key><string>ko</string>
  <key>CFBundleDisplayName</key><string>Runwick</string>
  <key>CFBundleExecutable</key><string>runwick</string>
  <key>CFBundleIconFile</key><string>AppIcon</string>
  <key>CFBundleIdentifier</key><string>dev.runwick.Runwick</string>
  <key>CFBundleInfoDictionaryVersion</key><string>6.0</string>
  <key>CFBundleName</key><string>Runwick</string>
  <key>CFBundlePackageType</key><string>APPL</string>
  <key>CFBundleShortVersionString</key><string>$VERSION</string>
  <key>CFBundleVersion</key><string>$VERSION</string>
  <key>LSApplicationCategoryType</key><string>public.app-category.developer-tools</string>
  <key>LSMinimumSystemVersion</key><string>11.0</string>
  <key>NSHighResolutionCapable</key><true/>
  <key>NSSupportsAutomaticGraphicsSwitching</key><true/>
</dict>
</plist>
EOF
plutil -lint "$APP/Contents/Info.plist" >/dev/null

# Nested code first (none today), then the bundle with its main executable. No entitlements: in
# particular none that allow DYLD environment variables or unsigned libraries.
SIGN="codesign --force --sign - --options runtime --timestamp=none"
for nested in "$APP"/Contents/Frameworks/* "$APP"/Contents/Helpers/* "$APP"/Contents/PlugIns/*; do
  if [ -e "$nested" ]; then $SIGN "$nested"; fi
done
$SIGN "$APP"
codesign --verify --strict --deep "$APP"
codesign -dv "$APP" 2>&1 | grep -q 'flags=.*runtime' || { echo "hardened runtime missing from the signature" >&2; exit 1; }

# Prove it: a library inserted through DYLD_INSERT_LIBRARIES must not run.
CHECK=$(mktemp -d "${TMPDIR:-/tmp}/runwick-inject.XXXXXX")
cat >"$CHECK/inject.c" <<'C'
#include <stdio.h>
#include <stdlib.h>
__attribute__((constructor)) static void injected(void) {
  const char *m = getenv("RUNWICK_INJECT_MARKER");
  FILE *f = m ? fopen(m, "w") : NULL;
  if (f) fclose(f);
}
C
if ! csrutil status 2>/dev/null | grep -q 'status: enabled'; then
  # Without System Integrity Protection (as on hosted CI runners) macOS does not enforce the
  # hardened runtime's DYLD restrictions, so the proof is meaningless here. The signature flag
  # was checked above; build on a Mac with SIP enabled to run the proof.
  echo "warning: System Integrity Protection is off, the injection check was skipped" >&2
elif cc -dynamiclib -o "$CHECK/inject.dylib" "$CHECK/inject.c" >/dev/null 2>&1; then
  RUNWICK_INJECT_MARKER="$CHECK/ran" DYLD_INSERT_LIBRARIES="$CHECK/inject.dylib" \
    "$APP/Contents/MacOS/runwick" --version >/dev/null 2>&1 || true
  if [ -e "$CHECK/ran" ]; then
    command rm -rf "$CHECK"
    echo "an inserted library ran inside the bundle: hardened runtime not effective" >&2
    exit 1
  fi
  echo "checked: DYLD_INSERT_LIBRARIES is ignored by the bundle"
else
  echo "warning: no C compiler, the injection check was skipped" >&2
fi
command rm -rf "$CHECK"
echo "built $APP ($VERSION)"
