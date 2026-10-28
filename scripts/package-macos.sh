#!/usr/bin/env bash
# Build a self-contained Apple Silicon app and drag-to-Applications disk image.
set -euo pipefail
cd "$(dirname "$0")/.."

if [ "$(uname -s)" != Darwin ]; then
  echo 'Packaging requires macOS (codesign, plutil, hdiutil).' >&2
  exit 1
fi
export MACOSX_DEPLOYMENT_TARGET=14.4
target=aarch64-apple-darwin
skip_build=false
case "${1:-}" in
  '') ;;
  --skip-build) skip_build=true ;;
  *) echo 'Usage: scripts/package-macos.sh [--skip-build]' >&2; exit 2 ;;
esac
if [ "$skip_build" = false ]; then
  cargo build --locked --release --target "$target" \
    -p notetaker-app -p notetaker-cli -p notetaker-asr-worker -p notetaker-embed-worker
fi
version=$(cargo metadata --locked --no-deps --format-version 1 | python3 -c 'import json,sys; print(next(p["version"] for p in json.load(sys.stdin)["packages"] if p["name"] == "notetaker-app"))')
metadata_target=$(cargo metadata --locked --no-deps --format-version 1 | python3 -c 'import json,sys; print(json.load(sys.stdin)["target_directory"])')
binaries="$metadata_target/$target/release"
out="$PWD/dist"
mkdir -p "$out"
staging=$(mktemp -d "${TMPDIR:-/tmp}/notetaker-package.XXXXXX")
trap 'rm -rf "$staging"' EXIT
app="$staging/notetaker.app"
mkdir -p "$app/Contents/MacOS" "$app/Contents/Resources"
for binary in notetaker-app notetaker-asr notetaker-embed notetaker; do
  test -x "$binaries/$binary"
  lipo -verify_arch arm64 "$binaries/$binary"
  cp "$binaries/$binary" "$app/Contents/MacOS/$binary"
  # Static sherpa/ORT must not leave machine-specific dylib dependencies behind.
  if otool -L "$app/Contents/MacOS/$binary" | tail -n +2 | awk '{print $1}' | grep -Ev '^(/System/Library/|/usr/lib/)' > "$staging/dependencies"; then
    echo "Unexpected non-system dependencies in $binary:" >&2
    cat "$staging/dependencies" >&2
    exit 1
  fi
  codesign --force --sign - "$app/Contents/MacOS/$binary"
done
python3 scripts/generate-icons.py --icns "$app/Contents/Resources/notetaker.icns"
cat > "$app/Contents/Info.plist" <<PLIST
<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0"><dict>
<key>CFBundleIdentifier</key><string>com.notetaker.app</string>
<key>CFBundleName</key><string>notetaker</string>
<key>CFBundleDisplayName</key><string>notetaker</string>
<key>CFBundleExecutable</key><string>notetaker-app</string>
<key>CFBundlePackageType</key><string>APPL</string>
<key>CFBundleVersion</key><string>$version</string>
<key>CFBundleShortVersionString</key><string>$version</string>
<key>CFBundleIconFile</key><string>notetaker.icns</string>
<key>LSMinimumSystemVersion</key><string>14.4</string>
<key>NSHighResolutionCapable</key><true/>
<key>NSMicrophoneUsageDescription</key><string>notetaker записывает ваш микрофон для локальной расшифровки встреч.</string>
<key>NSAudioCaptureUsageDescription</key><string>notetaker записывает звук собеседников для локальной расшифровки встреч.</string>
</dict></plist>
PLIST
cat > "$staging/audio.entitlements" <<'PLIST'
<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0"><dict>
<key>com.apple.security.device.audio-input</key><true/>
</dict></plist>
PLIST
plutil -lint "$app/Contents/Info.plist" "$staging/audio.entitlements"
codesign --force --deep --sign - --entitlements "$staging/audio.entitlements" "$app"
codesign --verify --deep --strict --verbose=2 "$app"
for binary in notetaker-app notetaker-asr notetaker-embed notetaker; do
  test -x "$app/Contents/MacOS/$binary"
done
test -s "$app/Contents/Resources/notetaker.icns"
ln -s /Applications "$staging/Applications"
# Keep build-only files out of the image.
image_root="$staging/image"
mkdir "$image_root"
mv "$app" "$staging/Applications" "$image_root/"
dmg="$out/notetaker-$version-macos-arm64.dmg"
hdiutil create -volname "notetaker $version" -srcfolder "$image_root" -ov -format UDZO "$dmg"
(cd "$out" && shasum -a 256 "$(basename "$dmg")" > "$(basename "$dmg").sha256")
if [ -n "${GITHUB_OUTPUT:-}" ]; then
  echo "dmg=$dmg" >> "$GITHUB_OUTPUT"
  echo "checksum=$dmg.sha256" >> "$GITHUB_OUTPUT"
fi
printf '%s\n' "$dmg" "$dmg.sha256"
