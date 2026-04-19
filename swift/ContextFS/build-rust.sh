#!/bin/bash
# Build Rust binaries (ctxfs + ctxfs-app-helper) and embed into ContextFS.app.
# Runs as an Xcode pre-build script phase on the ContextFS target.
set -euo pipefail

# Locate repo root — this script lives at swift/ContextFS/build-rust.sh
SCRIPT_DIR="$(cd "$(dirname "$0")" && pwd)"
REPO_ROOT="$(cd "$SCRIPT_DIR/../.." && pwd)"
cd "$REPO_ROOT"

# Ensure cargo is available. Xcode build phases may not inherit user PATH.
if ! command -v cargo >/dev/null 2>&1; then
    export PATH="$HOME/.cargo/bin:/opt/homebrew/bin:/usr/local/bin:$PATH"
fi
if ! command -v cargo >/dev/null 2>&1; then
    echo "error: cargo not found in PATH or $HOME/.cargo/bin" >&2
    exit 1
fi

# Determine build mode from Xcode's CONFIGURATION env var.
# CI Release builds: --release. Dev Debug builds: debug (faster compile).
if [ "${CONFIGURATION:-Debug}" = "Release" ]; then
    CARGO_FLAG="--release"
    TARGET_DIR="target/release"
else
    CARGO_FLAG=""
    TARGET_DIR="target/debug"
fi

echo "[build-rust.sh] cargo build $CARGO_FLAG -p ctxfs -p ctxfs-app-helper"
cargo build $CARGO_FLAG -p ctxfs -p ctxfs-app-helper

# Embed into the built .app bundle
DEST="${BUILT_PRODUCTS_DIR}/${PRODUCT_NAME}.app/Contents/MacOS"
mkdir -p "$DEST"

# Copy + preserve mtimes. cp -f overwrites any stale copy.
cp -f "$TARGET_DIR/ctxfs" "$DEST/ctxfs"
cp -f "$TARGET_DIR/ctxfs-app-helper" "$DEST/ctxfs-app-helper"

echo "[build-rust.sh] bundled ctxfs + ctxfs-app-helper into $DEST"
