#!/bin/sh
# Install a Contextful release binary: the full profile unless CONTEXTFUL_PROFILE names
# `contextful-edge` or `contextful-control` (`assurance.build.release-artifact`).
#
#   CONTEXTFUL_VERSION  the release to install, e.g. 0.5.0; required
#   CONTEXTFUL_PROFILE  the profile, default contextful-full
#   CONTEXTFUL_PREFIX   the directory receiving bin/contextful, default $HOME/.local
#   CONTEXTFUL_BASE_URL the release download root, default the repository's releases
#
# The archive's SHA-256 is checked against its published checksum before anything is
# installed.
set -eu

profile="${CONTEXTFUL_PROFILE:-contextful-full}"
version="${CONTEXTFUL_VERSION:?set CONTEXTFUL_VERSION to the release to install}"
prefix="${CONTEXTFUL_PREFIX:-$HOME/.local}"
base="${CONTEXTFUL_BASE_URL:-https://github.com/fractalboxdev/contextful/releases/download/v$version}"

case "$profile" in
  contextful-full | contextful-edge | contextful-control) ;;
  *) echo "install.sh: unknown profile $profile" >&2; exit 2 ;;
esac

case "$(uname -m)" in
  x86_64 | amd64) arch=x86_64 ;;
  arm64 | aarch64) arch=aarch64 ;;
  *) echo "install.sh: no release for machine $(uname -m)" >&2; exit 2 ;;
esac

case "$(uname -s)" in
  Linux) target="$arch-unknown-linux-musl" ;;
  Darwin)
    [ "$profile" = contextful-control ] && { echo "install.sh: contextful-control ships for Linux only" >&2; exit 2; }
    target="$arch-apple-darwin" ;;
  *) echo "install.sh: no release for $(uname -s)" >&2; exit 2 ;;
esac

stem="contextful-${profile#contextful-}-$version-$target"
archive="$stem.tar.gz"
work="$(mktemp -d)"
trap 'rm -rf "$work"' EXIT

fetch() {
  if command -v curl >/dev/null 2>&1; then curl -fsSL "$1" -o "$2"; else wget -q "$1" -O "$2"; fi
}

fetch "$base/$archive" "$work/$archive"
fetch "$base/$archive.sha256" "$work/$archive.sha256"
expected="$(cut -d' ' -f1 "$work/$archive.sha256")"
if command -v sha256sum >/dev/null 2>&1; then
  actual="$(sha256sum "$work/$archive" | cut -d' ' -f1)"
else
  actual="$(shasum -a 256 "$work/$archive" | cut -d' ' -f1)"
fi
[ "$expected" = "$actual" ] || { echo "install.sh: $archive checksum $actual differs from $expected" >&2; exit 1; }

tar -xzf "$work/$archive" -C "$work"
mkdir -p "$prefix/bin"
install -m 0755 "$work/$stem/contextful" "$prefix/bin/contextful"
echo "installed $profile $version at $prefix/bin/contextful"
