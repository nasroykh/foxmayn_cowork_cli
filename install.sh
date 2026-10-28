#!/usr/bin/env sh
set -eu

REPO="nasroykh/foxmayn_cowork_cli"
BINARY="foxmayn-cowork"

# --- auth header ---
# Opt-in only: set GITHUB_TOKEN (needed for private repos or API rate limits).
AUTH_HEADER=""
if [ -n "${GITHUB_TOKEN:-}" ]; then
  AUTH_HEADER="Authorization: token ${GITHUB_TOKEN}"
fi

# Helper: curl with optional auth
fetch() {
  if [ -n "$AUTH_HEADER" ]; then
    curl -fsSL -H "$AUTH_HEADER" "$@"
  else
    curl -fsSL "$@"
  fi
}

# --- detect OS ---
OS=$(uname -s | tr '[:upper:]' '[:lower:]')
case "$OS" in
  linux)  OS="linux" ;;
  darwin) OS="darwin" ;;
  *)
    echo "Unsupported OS: $OS" >&2
    exit 1
    ;;
esac

# --- detect arch ---
ARCH=$(uname -m)
case "$ARCH" in
  x86_64 | amd64) ARCH="x86_64" ;;
  arm64 | aarch64) ARCH="aarch64" ;;
  *)
    echo "Unsupported architecture: $ARCH" >&2
    exit 1
    ;;
esac

# --- map to Rust target triple ---
case "${OS}_${ARCH}" in
  linux_x86_64)   TARGET="x86_64-unknown-linux-gnu" ;;
  linux_aarch64)  TARGET="aarch64-unknown-linux-gnu" ;;
  darwin_x86_64)  TARGET="x86_64-apple-darwin" ;;
  darwin_aarch64) TARGET="aarch64-apple-darwin" ;;
  *)
    echo "No pre-built binary for ${OS}/${ARCH}" >&2
    exit 1
    ;;
esac

# --- resolve latest release tag ---
# Override with FOXMAYN_VERSION=vX.Y.Z to pin a version (also avoids API rate limits).
VERSION="${FOXMAYN_VERSION:-}"
if [ -z "$VERSION" ]; then
  RELEASE_JSON=$(fetch "https://api.github.com/repos/${REPO}/releases/latest" || true)
  VERSION=$(printf '%s\n' "$RELEASE_JSON" \
    | grep '"tag_name"' \
    | sed 's/.*"tag_name": *"\([^"]*\)".*/\1/' || true)
fi

if [ -z "$VERSION" ]; then
  echo "Could not determine latest release version." >&2
  echo "Set FOXMAYN_VERSION=vX.Y.Z to pin a version, or set GITHUB_TOKEN" >&2
  echo "if you hit API rate limits or the repo is private." >&2
  exit 1
fi

echo "Installing ${BINARY} ${VERSION} (${OS}/${ARCH})..."

ARCHIVE="${BINARY}_${VERSION}_${TARGET}.tar.gz"
URL="https://github.com/${REPO}/releases/download/${VERSION}/${ARCHIVE}"
CHECKSUM_URL="https://github.com/${REPO}/releases/download/${VERSION}/${BINARY}_${VERSION}_${TARGET}.sha256"

TMP=$(mktemp -d)
trap 'rm -rf "$TMP"' EXIT

# --- download archive + checksum ---
fetch "$URL" -o "$TMP/$ARCHIVE"
fetch "$CHECKSUM_URL" -o "$TMP/${ARCHIVE}.sha256"

# --- verify checksum ---
cd "$TMP"
if command -v sha256sum > /dev/null 2>&1; then
  sha256sum -c "${ARCHIVE}.sha256"
elif command -v shasum > /dev/null 2>&1; then
  shasum -a 256 -c "${ARCHIVE}.sha256"
elif [ "${FOXMAYN_COWORK_SKIP_CHECKSUM:-}" = "1" ]; then
  echo "Warning: no sha256 tool found, FOXMAYN_COWORK_SKIP_CHECKSUM=1 set: skipping verification." >&2
else
  echo "Error: no sha256sum or shasum found; cannot verify download." >&2
  echo "Install one, or set FOXMAYN_COWORK_SKIP_CHECKSUM=1 to skip (not recommended)." >&2
  exit 1
fi
cd - > /dev/null

# --- extract ---
tar -xzf "$TMP/$ARCHIVE" -C "$TMP"

# --- install ---
INSTALL_DIR=""
if [ -w "/usr/local/bin" ]; then
  INSTALL_DIR="/usr/local/bin"
elif [ -d "$HOME/.local/bin" ]; then
  INSTALL_DIR="$HOME/.local/bin"
else
  mkdir -p "$HOME/.local/bin"
  INSTALL_DIR="$HOME/.local/bin"
fi

mv "$TMP/$BINARY" "$INSTALL_DIR/$BINARY"
chmod +x "$INSTALL_DIR/$BINARY"

echo ""
echo "Installed to $INSTALL_DIR/$BINARY"

# Warn if install dir is not on PATH
case ":$PATH:" in
  *":$INSTALL_DIR:"*) ;;
  *)
    echo ""
    echo "Note: $INSTALL_DIR is not in your PATH."
    echo "Add this to your shell profile:"
    echo "  export PATH=\"\$PATH:$INSTALL_DIR\""
    ;;
esac

echo "Run '${BINARY} --help' to get started."
