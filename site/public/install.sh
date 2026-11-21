#!/usr/bin/env bash
set -euo pipefail

# Installer for sf-source-format (busbar-sf-source-format)
# Repository: https://github.com/composable-delivery/sf-source-format

REPO="composable-delivery/sf-source-format"
INSTALL_DIR="${INSTALL_DIR:-$HOME/.local/bin}"

OS="$(uname -s)"
case "$OS" in
  Linux*)  TARBALL="sf-source-format-linux-x86_64.tar.gz" ;;
  Darwin*) TARBALL="sf-source-format-macos-universal.tar.gz" ;;
  *)
    echo "Error: Unsupported operating system: $OS" >&2
    echo "On Windows, use: irm https://docs.muselab.com/busbar-sf-source-format/install.ps1 | iex" >&2
    exit 1
    ;;
esac

DOWNLOAD_URL="https://github.com/${REPO}/releases/latest/download/${TARBALL}"

echo "Installing sf-source-format ($TARBALL)..."

TMP_DIR="$(mktemp -d)"
cleanup() { rm -rf "$TMP_DIR"; }
trap cleanup EXIT

curl -fsSL "$DOWNLOAD_URL" -o "$TMP_DIR/$TARBALL"
mkdir -p "$INSTALL_DIR"
tar -xzf "$TMP_DIR/$TARBALL" -C "$TMP_DIR"
mv "$TMP_DIR/sf-source-format" "$INSTALL_DIR/sf-source-format"
chmod +x "$INSTALL_DIR/sf-source-format"

echo "Successfully installed sf-source-format to $INSTALL_DIR/sf-source-format"

if ! echo ":$PATH:" | grep -q ":$INSTALL_DIR:"; then
  echo ""
  echo "Add $INSTALL_DIR to your PATH by adding this line to your shell profile (~/.bashrc or ~/.zshrc):"
  echo "  export PATH=\"\$HOME/.local/bin:\$PATH\""
fi

echo ""
echo "Verify installation with:"
echo "  sf-source-format --help"
