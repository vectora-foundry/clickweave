#!/usr/bin/env bash
set -euo pipefail

REPO="anthropics/native-devtools-mcp"
VERSION=$(cat "$(dirname "$0")/../NATIVE_DEVTOOLS_VERSION")
CHECKSUMS="$(dirname "$0")/../checksums.txt"
OUT_DIR="$(dirname "$0")/../src-tauri/binaries"

# Detect target triple
TARGET="${TARGET:-$(rustc --print host-tuple 2>/dev/null || rustc -vV | sed -n 's/^host: //p')}"

echo "Fetching native-devtools-mcp v${VERSION} for ${TARGET}..."

mkdir -p "$OUT_DIR"

if [[ "$TARGET" == *"windows"* ]]; then
    ARCHIVE="native-devtools-mcp-${TARGET}.zip"
    BINARY="native-devtools-mcp-${TARGET}.exe"
else
    ARCHIVE="native-devtools-mcp-${TARGET}.tar.gz"
    BINARY="native-devtools-mcp-${TARGET}"
fi

URL="https://github.com/${REPO}/releases/download/v${VERSION}/${ARCHIVE}"

# Download
echo "Downloading ${URL}..."
curl -fSL -o "/tmp/${ARCHIVE}" "$URL"

# Verify checksum
if [ -f "$CHECKSUMS" ]; then
    EXPECTED=$(grep "${ARCHIVE}" "$CHECKSUMS" | awk '{print $1}' || true)
    if [ -n "$EXPECTED" ]; then
        ACTUAL=$(shasum -a 256 "/tmp/${ARCHIVE}" | awk '{print $1}')
        if [ "$ACTUAL" != "$EXPECTED" ]; then
            echo "ERROR: Checksum mismatch for ${ARCHIVE}"
            echo "  Expected: ${EXPECTED}"
            echo "  Actual:   ${ACTUAL}"
            rm -f "/tmp/${ARCHIVE}"
            exit 1
        fi
        echo "Checksum verified."
    else
        echo "WARNING: No checksum found for ${ARCHIVE}, skipping verification."
    fi
fi

# Extract
if [[ "$ARCHIVE" == *.tar.gz ]]; then
    tar -xzf "/tmp/${ARCHIVE}" -C "$OUT_DIR"
    # The archive contains the raw binary — rename to include target triple
    mv "$OUT_DIR/native-devtools-mcp" "$OUT_DIR/$BINARY" 2>/dev/null || true
else
    unzip -o "/tmp/${ARCHIVE}" -d "$OUT_DIR"
    mv "$OUT_DIR/native-devtools-mcp.exe" "$OUT_DIR/$BINARY" 2>/dev/null || true
fi

chmod +x "$OUT_DIR/$BINARY"
rm -f "/tmp/${ARCHIVE}"

echo "Sidecar binary ready: ${OUT_DIR}/${BINARY}"
