#!/usr/bin/env bash
set -euo pipefail

ROOT="$(cd "$(dirname "$0")" && pwd)"
FRONTEND_DIR="$ROOT/frontend"
BACKEND_DIR="$ROOT"

echo "=== RCUI Build ==="

# ─── Frontend ─────────────────────────────────────────────────────────────────
echo ""
echo ">>> Building frontend..."

cd "$FRONTEND_DIR"

if [ ! -d "node_modules" ]; then
    echo "    Installing npm dependencies..."
    npm install
fi

echo "    Running vite build..."
npm run build

DIST_DIR="$FRONTEND_DIR/dist"
if [ ! -d "$DIST_DIR" ]; then
    echo "ERROR: Frontend build failed - dist/ not found"
    exit 1
fi
echo "    Frontend built to $DIST_DIR"

# ─── Backend ──────────────────────────────────────────────────────────────────
echo ""
echo ">>> Building Rust backend..."

cd "$BACKEND_DIR"

PROFILE="${1:-release}"

if [ "$PROFILE" = "release" ]; then
    cargo build --release
    BINARY="$ROOT/target/release/rcui-server"
else
    cargo build
    BINARY="$ROOT/target/debug/rcui-server"
fi

if [ ! -f "$BINARY" ]; then
    echo "ERROR: Backend build failed - binary not found"
    exit 1
fi
echo "    Backend built: $BINARY"

# ─── Done ─────────────────────────────────────────────────────────────────────
echo ""
echo "=== Build complete ==="
echo ""
echo "To start the server:"
echo "  STATIC_DIR=$DIST_DIR $BINARY"
echo ""
echo "Or for development:"
echo "  cd frontend && npm run dev   # in one terminal"
echo "  cargo run                    # in another terminal"
