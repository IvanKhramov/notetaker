#!/usr/bin/env bash
# Download sherpa-onnx prebuilt static libraries outside target/ and export
# SHERPA_ONNX_LIB_DIR. rust-cache prunes target/ but restores build-script
# fingerprints, so relying on sherpa-onnx-sys's own download breaks linking.
set -euo pipefail

version=$(awk '/^name = "sherpa-onnx-sys"$/ {getline; gsub(/"/, "", $3); print $3}' Cargo.lock)
case "${RUNNER_OS:-$(uname -s)}" in
  Linux) stem="sherpa-onnx-v${version}-linux-x64-static-lib" ;;
  macOS | Darwin)
    if [ "$(uname -m)" = "arm64" ]; then
      stem="sherpa-onnx-v${version}-osx-arm64-static-lib"
    else
      stem="sherpa-onnx-v${version}-osx-x64-static-lib"
    fi
    ;;
  Windows) stem="sherpa-onnx-v${version}-win-x64-static-MT-Release-lib" ;;
  *) echo "unsupported runner OS" >&2; exit 1 ;;
esac

root="${SHERPA_LIBS_ROOT:-$HOME/sherpa-onnx-libs}"
lib_dir="$root/$stem/lib"
if [ ! -d "$lib_dir" ]; then
  mkdir -p "$root"
  curl -fsSL --retry 5 -o "$root/$stem.tar.bz2" \
    "https://github.com/k2-fsa/sherpa-onnx/releases/download/v${version}/${stem}.tar.bz2"
  tar -xjf "$root/$stem.tar.bz2" -C "$root"
  rm "$root/$stem.tar.bz2"
fi
test -d "$lib_dir"

# Cargo on Windows needs a native path, not the Git Bash one.
if command -v cygpath > /dev/null; then
  lib_dir=$(cygpath -w "$lib_dir")
fi
if [ -n "${GITHUB_ENV:-}" ]; then
  echo "SHERPA_ONNX_LIB_DIR=$lib_dir" >> "$GITHUB_ENV"
fi
echo "SHERPA_ONNX_LIB_DIR=$lib_dir"
