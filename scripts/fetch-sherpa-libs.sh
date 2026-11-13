#!/usr/bin/env bash
# Download sherpa-onnx prebuilt static libraries outside target/ and export
# SHERPA_ONNX_LIB_DIR. rust-cache prunes target/ but restores build-script
# fingerprints, so relying on sherpa-onnx-sys's own download breaks linking.
set -euo pipefail

version=$(awk '/^name = "sherpa-onnx-sys"$/ {getline; gsub(/"/, "", $3); print $3}' Cargo.lock)
case "${RUNNER_OS:-$(uname -s)}" in
  Linux) stem="sherpa-onnx-v${version}-linux-x64-static-lib" ;;
  macOS | Darwin)
    if [ "${SHERPA_TARGET_ARCH:-$(uname -m)}" = "arm64" ]; then
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
  # These libraries are linked into release binaries. After a sherpa-onnx bump,
  # add the new archive hashes here; an unknown archive is refused.
  case "$stem" in
    sherpa-onnx-v2.0.0-linux-x64-static-lib) expected=d9879a4b8b6abc6ad4168887d7428776c2eb04646b19b744e1307a597555bc17 ;;
    sherpa-onnx-v2.0.0-osx-arm64-static-lib) expected=5e5bdce1d7fc834857278d8bb26f878de6a55c7085e589ed1e41a00baca643ec ;;
    sherpa-onnx-v2.0.0-osx-x64-static-lib) expected=a6096bb89cb92e071f5fca4a9a869f7251587b707dd52852adc1a8ad6ace9051 ;;
    sherpa-onnx-v2.0.0-win-x64-static-MT-Release-lib) expected=13cbef1926eea3bc01c8eb59bbfa3806f02f904a76664c215c4e20989866704a ;;
    *) echo "no pinned sha256 for $stem" >&2; rm "$root/$stem.tar.bz2"; exit 1 ;;
  esac
  if command -v sha256sum > /dev/null; then
    actual=$(sha256sum "$root/$stem.tar.bz2" | cut -d' ' -f1)
  else
    actual=$(shasum -a 256 "$root/$stem.tar.bz2" | cut -d' ' -f1)
  fi
  if [ "$actual" != "$expected" ]; then
    echo "sha256 mismatch for $stem: $actual" >&2
    rm "$root/$stem.tar.bz2"
    exit 1
  fi
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
