#!/bin/bash
# Builds FishBot in release mode and packages it into ~/Desktop/FishBot.app,
# signing every binary with a FIXED identifier so macOS's Screen Recording
# permission grant survives rebuilds.
#
# Why this exists: cargo's own build never code-signs anything -- on Apple
# Silicon, ld auto-adhoc-signs each binary at link time with an identifier
# derived from a hash of its own content (e.g. "control_panel-099bda09e7ac"),
# which changes on every single build. macOS's TCC permission database keys
# Screen Recording grants to that exact identifier, so every rebuild silently
# invalidates the grant even though the old entry stays listed (toggled on,
# doing nothing) in System Settings. Re-signing each binary here with an
# explicit --identifier overrides that unstable one with a fixed one, so a
# grant made against these identifiers keeps working across rebuilds.
#
# fisch_new/calibrate_rod/region_overlay/point_overlay are spawned as plain
# subprocesses (not through the .app's own identity), so macOS treats each
# as its own distinct app for permission purposes -- every one needs this,
# not just control_panel.

set -euo pipefail

WORKSPACE_ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
APP="$HOME/Desktop/FishBot.app"
DEST="$APP/Contents/MacOS"
BUNDLE_ID="com.aedinlai.fishbot"

echo "==> Building release binaries..."
(cd "$WORKSPACE_ROOT/fish_bar_rust" && cargo build --release)

SRC="$WORKSPACE_ROOT/target/release"

echo "==> Stopping any running FishBot helpers..."
for name in control_panel fisch_new calibrate_rod region_overlay point_overlay; do
    pkill -f "^$DEST/$name( |$)" 2>/dev/null || true
done

echo "==> Copying binaries into the bundle..."
mkdir -p "$DEST"
for name in control_panel fisch_new calibrate_rod region_overlay point_overlay; do
    cp "$SRC/$name" "$DEST/$name"
    chmod +x "$DEST/$name"
done
cp -f "$WORKSPACE_ROOT/fish_bar_rust/moon.png" "$DEST/moon.png" 2>/dev/null || true
cp -f "$WORKSPACE_ROOT/fish_bar_rust/sun.png" "$DEST/sun.png" 2>/dev/null || true

echo "==> Stripping Finder/quarantine metadata (codesign refuses to seal a bundle with it present)..."
xattr -cr "$APP"

echo "==> Signing with fixed identifiers (ad-hoc, but stable across rebuilds)..."
# Each binary is signed individually rather than re-sealing the whole .app
# with --deep: macOS's TCC keys Screen Recording grants to each spawned
# EXECUTABLE's own identity anyway (these run as raw subprocesses, not
# through the .app's identity), so that's what actually needs to be stable.
# A whole-bundle --deep reseal would be nice for Gatekeeper/spctl coherence,
# but a com.apple.provenance attribute macOS attaches to locally-built files
# (and won't let a plain script strip) blocks it -- not worth fighting for
# a purely cosmetic win.
for name in fisch_new calibrate_rod region_overlay point_overlay; do
    codesign --force --sign - --identifier "$BUNDLE_ID.$name" "$DEST/$name"
done
# control_panel is special: codesign recognizes it as this bundle's
# CFBundleExecutable (per Info.plist) and switches into full bundle-sealing
# mode, which then trips over the stray .pngs sitting in MacOS/ (Apple's
# convention wants those in Contents/Resources/ instead) plus Finder
# metadata churn on the .app directory. Signing a bare copy OUTSIDE the
# bundle avoids that detection entirely -- the signature lives embedded in
# the Mach-O itself, so copying the already-signed binary back in place
# afterward is just as valid as signing it in place would have been.
TMP_SIGN="$(mktemp -d)"
cp "$DEST/control_panel" "$TMP_SIGN/control_panel"
codesign --force --sign - --identifier "$BUNDLE_ID" "$TMP_SIGN/control_panel"
cp "$TMP_SIGN/control_panel" "$DEST/control_panel"
chmod +x "$DEST/control_panel"
rm -rf "$TMP_SIGN"

echo "==> Done. Signing identities:"
for name in control_panel fisch_new calibrate_rod region_overlay point_overlay; do
    id=$(codesign -dv "$DEST/$name" 2>&1 | grep '^Identifier=')
    echo "  $name: $id"
done

echo ""
echo "If macOS still shows stale Screen Recording entries for the old"
echo "per-build identifiers, remove them in System Settings > Privacy &"
echo "Security > Screen Recording, then re-grant for the 5 entries above"
echo "the next time each one prompts. That grant should now survive future"
echo "rebuilds via this script, since the identifier no longer changes."
