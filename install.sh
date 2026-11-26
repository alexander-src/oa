#!/usr/bin/env sh
set -eu

REPO="alexander-src/oa"
INSTALL_DIR="${OA_INSTALL_DIR:-$HOME/.local/bin}"

OS="$(uname -s)"
ARCH="$(uname -m)"

case "$OS" in
  Linux)
    case "$ARCH" in
      x86_64|amd64) ASSET="oa-linux-x86_64.tar.gz" ;;
      *)
        echo "error: unsupported Linux architecture '$ARCH' (build from source with 'cargo install --git https://github.com/$REPO')" >&2
        exit 1
        ;;
    esac
    ;;
  Darwin)
    case "$ARCH" in
      arm64|aarch64) ASSET="oa-macos-arm64.tar.gz" ;;
      *)
        echo "error: unsupported macOS architecture '$ARCH' (build from source with 'cargo install --git https://github.com/$REPO')" >&2
        exit 1
        ;;
    esac
    ;;
  *)
    echo "error: unsupported OS '$OS' (Windows binaries are on https://github.com/$REPO/releases)" >&2
    exit 1
    ;;
esac

BASE_URL="https://github.com/$REPO/releases/latest/download"
TMP_DIR="$(mktemp -d)"
trap 'rm -rf "$TMP_DIR"' EXIT

echo "Downloading $ASSET from $REPO latest release..."
curl -fsSL "$BASE_URL/$ASSET" -o "$TMP_DIR/$ASSET"
curl -fsSL "$BASE_URL/SHA256SUMS.txt" -o "$TMP_DIR/SHA256SUMS.txt"

if command -v sha256sum >/dev/null 2>&1; then
  (cd "$TMP_DIR" && grep " $ASSET\$" SHA256SUMS.txt | sha256sum -c -)
elif command -v shasum >/dev/null 2>&1; then
  (cd "$TMP_DIR" && grep " $ASSET\$" SHA256SUMS.txt | shasum -a 256 -c -)
fi

mkdir -p "$INSTALL_DIR"
tar -xzf "$TMP_DIR/$ASSET" -C "$TMP_DIR"
install -m 755 "$TMP_DIR/oa" "$INSTALL_DIR/oa"

echo "Installed oa to $INSTALL_DIR/oa"
case ":$PATH:" in
  *":$INSTALL_DIR:"*) ;;
  *)
    echo "Add '$INSTALL_DIR' to your PATH, then run: oa setup"
    ;;
esac
