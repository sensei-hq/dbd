#!/usr/bin/env bash
set -euo pipefail

VERSION=${1:?usage: ./bump-version.sh X.Y.Z}

# Update workspace version in Cargo.toml
sed -i '' "s/^version = \"[^\"]*\"/version = \"$VERSION\"/" Cargo.toml

# Rebuild lock file
cargo check -q 2>&1 | grep -v "^$" || true

git add Cargo.toml Cargo.lock
git commit -m "chore: bump version to v$VERSION"
git tag "v$VERSION"

echo "Tagged v$VERSION — push with: git push && git push --tags"
