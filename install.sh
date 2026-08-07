#!/bin/sh
# DeepseekNova CLI — one-line installer (POSIX sh)
#
# Usage:
#   curl -fsSL https://raw.githubusercontent.com/W117C/DeepseekNova/main/install.sh | sh
#   sh install.sh                          # latest GitHub release
#   sh install.sh 0.4.0                    # explicit version (v-prefix optional)
#   INSTALL_DIR=/custom/dir sh install.sh 0.4.0   # custom install dir
#
# Downloads the release binary for the current platform, verifies its SHA-256
# against the release's checksums.txt, then installs it to INSTALL_DIR.
#
# Naming contract (must match .github/workflows/release.yml):
#   asset    = deepseeknova-cli-<target>.tar.gz  /  deepseeknova-cli-<target>.zip
#   checksums.txt lines = "<sha256hex>  <path>" (sha256sum output, double space)
#   binaries inside archives: deepseeknova-cli (unix) / deepseeknova-cli.exe (windows)

set -eu

# ---------------------------------------------------------------------------
# Configuration
# ---------------------------------------------------------------------------
REPO="W117C/DeepseekNova"
RELEASE_BASE="https://github.com/$REPO/releases/download"
API_LATEST="https://api.github.com/repos/$REPO/releases/latest"
: "${INSTALL_DIR:=${HOME:-/}/.local/bin}"

# ---------------------------------------------------------------------------
# Helpers
# ---------------------------------------------------------------------------
say() { printf '%s\n' "$*"; }
die() { printf 'error: %s\n' "$*" >&2; exit 1; }

command -v curl >/dev/null 2>&1 || die "curl is required but was not found in PATH"

# Work happens in a temp dir that is always cleaned up on exit (success or failure).
tmpdir="$(mktemp -d "${TMPDIR:-/tmp}/dsn-install.XXXXXX")"
trap 'rm -rf "$tmpdir"' EXIT

# ---------------------------------------------------------------------------
# Resolve version -> release tag
# ---------------------------------------------------------------------------
if [ "$#" -ge 1 ]; then
  version="$1"
  case "$version" in
    v*) tag="$version" ;;
    *)  tag="v$version" ;;
  esac
  case "$tag" in
    v[0-9]*) ;;
    *) die "invalid version '$version' (expected something like 0.4.0)" ;;
  esac
else
  say "Resolving latest release from GitHub API: $API_LATEST"
  json="$(curl -fsSL "$API_LATEST")" \
    || die "failed to query GitHub API for the latest release ($API_LATEST)"
  tag="$(printf '%s\n' "$json" | sed -n 's/.*"tag_name"[[:space:]]*:[[:space:]]*"\([^"]*\)".*/\1/p' | head -n 1)"
  [ -n "$tag" ] || die "could not parse tag_name from GitHub API response"
  say "Latest release tag: $tag"
fi

# ---------------------------------------------------------------------------
# Platform detection -> Rust target triple
# ---------------------------------------------------------------------------
uname_s="$(uname -s 2>/dev/null || echo unknown)"
uname_m="$(uname -m 2>/dev/null || echo unknown)"

unsupported() {
  die "unsupported platform: $1
supported platforms:
  macOS   aarch64 (Apple Silicon) / x86_64 (Intel)
  Linux   aarch64 / x86_64
  Windows x86_64  -> use install.ps1:
      irm https://raw.githubusercontent.com/W117C/DeepseekNova/main/install.ps1 | iex"
}

case "$uname_s" in
  Darwin)
    os=macos
    case "$uname_m" in
      aarch64 | arm64) target="aarch64-apple-darwin" ;;
      x86_64 | amd64)  target="x86_64-apple-darwin" ;;
      *) unsupported "macOS $uname_m" ;;
    esac
    ;;
  Linux)
    os=linux
    case "$uname_m" in
      aarch64 | arm64) target="aarch64-unknown-linux-gnu" ;;
      x86_64 | amd64)  target="x86_64-unknown-linux-gnu" ;;
      *) unsupported "Linux $uname_m" ;;
    esac
    ;;
  *)
    unsupported "$uname_s"
    ;;
esac

asset="deepseeknova-cli-$target.tar.gz"
url="$RELEASE_BASE/$tag/$asset"
say "Platform: $os/$uname_m  ->  target: $target"
say "Downloading $url"

# ---------------------------------------------------------------------------
# Download asset + checksums.txt
# ---------------------------------------------------------------------------
curl -fsSL -o "$tmpdir/$asset" "$url" \
  || die "failed to download $url (does release $tag include $asset?)"
curl -fsSL -o "$tmpdir/checksums.txt" "$RELEASE_BASE/$tag/checksums.txt" \
  || die "failed to download checksums.txt for release $tag"
[ -s "$tmpdir/checksums.txt" ] || die "checksums.txt for release $tag is empty or missing"

# ---------------------------------------------------------------------------
# SHA-256 verification — match by asset basename only (checksums.txt paths have
# a subdirectory prefix, e.g. "./<artifact-dir>/<asset>")
# ---------------------------------------------------------------------------
case "$os" in
  macos)
    command -v shasum >/dev/null 2>&1 || die "shasum not found (needed for SHA-256 verification)"
    hash_cmd="shasum -a 256"
    ;;
  *)
    command -v sha256sum >/dev/null 2>&1 || die "sha256sum not found (needed for SHA-256 verification)"
    hash_cmd="sha256sum"
    ;;
esac

actual_hash="$($hash_cmd "$tmpdir/$asset" | awk '{ print $1 }')"
[ -n "$actual_hash" ] || die "failed to compute SHA-256 of the downloaded archive"

expected_hash="$(awk -v want="$asset" '
  function basename(p, n, parts) { n = split(p, parts, "/"); return parts[n] }
  basename($2) == want { print $1; exit }
' "$tmpdir/checksums.txt")"

if [ "${#expected_hash}" -ne 64 ]; then
  die "no SHA-256 entry for $asset in checksums.txt (release $tag may not contain it)"
fi

if [ "$actual_hash" != "$expected_hash" ]; then
  say "checksum MISMATCH for $asset"
  say "  expected: $expected_hash"
  say "  actual:   $actual_hash"
  die "integrity check FAILED — refusing to install (downloaded file removed). Retry or check the release."
fi
say "Checksum OK: $asset"

# ---------------------------------------------------------------------------
# Install
# ---------------------------------------------------------------------------
tar -xzf "$tmpdir/$asset" -C "$tmpdir"
[ -f "$tmpdir/deepseeknova-cli" ] \
  || die "unexpected archive layout: deepseeknova-cli not found in $asset"

mkdir -p "$INSTALL_DIR"
cp "$tmpdir/deepseeknova-cli" "$INSTALL_DIR/deepseeknova-cli"
chmod +x "$INSTALL_DIR/deepseeknova-cli"
say "Installed DeepseekNova CLI ($tag) to $INSTALL_DIR/deepseeknova-cli"

# ---------------------------------------------------------------------------
# PATH hint
# ---------------------------------------------------------------------------
case ":$PATH:" in
  *":$INSTALL_DIR:"*) path_ok=1 ;;
  *) path_ok=0 ;;
esac

if [ "$path_ok" -eq 0 ]; then
  say ""
  say "NOTE: $INSTALL_DIR is not in your PATH."
  say "  Add it to your shell configuration (e.g. ~/.zshrc or ~/.bashrc), then restart your shell:"
  say "    export PATH=\"$INSTALL_DIR:\$PATH\""
  say "  Or run it directly now: $INSTALL_DIR/deepseeknova-cli --version"
else
  say "DeepseekNova CLI is ready. Run 'deepseeknova-cli --version' to confirm."
fi
