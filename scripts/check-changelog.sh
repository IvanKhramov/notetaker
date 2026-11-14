#!/usr/bin/env bash
set -euo pipefail
version=$(sed -n 's/^version = "\([^"]*\)"/\1/p' Cargo.toml | head -1)
test -n "$version"
grep -Fq "## [$version] - " CHANGELOG.md
