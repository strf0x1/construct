#!/bin/sh
# Construct installer
#
#   curl -fsSL https://construct.sprawlcommons.com/install.sh | sh
#   curl -fsSL https://construct.sprawlcommons.com/install.sh | sh -s -- --version v0.1.0
#
# What it does, and nothing else:
#   1. checks this is a supported platform (Linux x86-64, macOS arm64 or x86-64)
#   2. resolves one release version: the latest, or the one you ask for
#   3. downloads that release's archive and its SHA-256 checksum from GitHub
#   4. verifies the checksum, and that the binary runs here, before touching
#      anything you already have
#   5. puts `construct` in ~/.local/bin (or --dir), replacing an old copy in
#      one atomic rename
#
# It never uses sudo, never edits your shell profile, configures no models,
# and talks to nothing but github.com. If any step fails, it stops and your
# existing install is left as it was.
#
# Prefer to do it by hand? Every release lists its archive and checksum:
#   https://github.com/strf0x1/construct/releases

set -eu

REPO="strf0x1/construct"

say() { printf 'construct: %s\n' "$*" >&2; }
err() { say "error: $*"; exit 1; }

usage() {
    cat >&2 <<'EOF'
Install construct from a GitHub release.

usage: install.sh [--version vX.Y.Z] [--dir PATH]

  --version vX.Y.Z  install this release instead of the latest
                    (also: CONSTRUCT_VERSION; use it to roll back)
  --dir PATH        install into PATH (default: ~/.local/bin;
                    also: CONSTRUCT_INSTALL_DIR)
  -h, --help        show this help

With curl | sh, pass options after `sh -s --`:
  curl -fsSL https://construct.sprawlcommons.com/install.sh | sh -s -- --version v0.1.0
EOF
}

need() { command -v "$1" >/dev/null 2>&1 || err "this installer needs '$1', which isn't on PATH"; }

# fetch URL FILE
fetch() {
    if [ "$downloader" = curl ]; then
        curl --proto '=https' --tlsv1.2 -fsSL --retry 3 -o "$2" "$1"
    else
        wget --https-only -q --tries=3 -O "$2" "$1"
    fi
}

# The tag GitHub's /releases/latest redirects to. Drafts and pre-releases
# are never "latest".
latest_version() {
    page="https://github.com/$REPO/releases/latest"
    if [ "$downloader" = curl ]; then
        url=$(curl --proto '=https' --tlsv1.2 -fsSL --retry 3 -o /dev/null -w '%{url_effective}' "$page") ||
            err "couldn't reach $page (no published release yet, or no network?)"
    else
        url=$(wget --https-only -q --max-redirect=0 -S -O /dev/null "$page" 2>&1 |
            sed -n 's/^ *[Ll]ocation: *//p' | tail -n 1 | tr -d '\r') || true
    fi
    tag=${url##*/}
    case "$tag" in
        v[0-9]*) printf '%s\n' "$tag" ;;
        *) err "couldn't work out the latest release from $page (is one published?)" ;;
    esac
}

sha256() {
    if command -v sha256sum >/dev/null 2>&1; then
        sha256sum "$1" | awk '{print $1}'
    elif command -v shasum >/dev/null 2>&1; then
        shasum -a 256 "$1" | awk '{print $1}'
    else
        err "this installer needs 'sha256sum' or 'shasum' to verify the download"
    fi
}

check_platform() {
    os=$(uname -s)
    arch=$(uname -m)
    case "$os" in
        Linux) check_linux ;;
        Darwin) check_macos ;;
        *) err "construct runs on Linux and macOS; this is $os" ;;
    esac
}

check_linux() {
    case "$arch" in
        x86_64 | amd64) target=x86_64-unknown-linux-gnu ;;
        aarch64 | arm64) err "Linux ARM64 builds aren't published yet; build from source: https://github.com/$REPO" ;;
        *) err "no build for $arch yet; build from source: https://github.com/$REPO" ;;
    esac

    if ldd --version 2>&1 | grep -qi musl; then
        err "musl-based systems (such as Alpine) aren't supported yet; release builds need glibc"
    fi

    # Landlock arrived in Linux 5.13. Older kernels can run the binary but not
    # the sandbox, so warn rather than stop.
    kernel=$(uname -r)
    major=${kernel%%.*}
    rest=${kernel#*.}
    minor=${rest%%[!0-9]*}
    case "$major$minor" in
        *[!0-9]* | '') ;;
        *)
            if [ "$major" -lt 5 ] || { [ "$major" -eq 5 ] && [ "$minor" -lt 13 ]; }; then
                say "warning: kernel $kernel predates Landlock (5.13); the tool sandbox won't be available"
            fi
            ;;
    esac
}

check_macos() {
    # A shell running under Rosetta reports x86_64 on an Apple silicon Mac;
    # the native build is what you want there.
    if [ "$arch" = x86_64 ] && [ "$(sysctl -n hw.optional.arm64 2>/dev/null || true)" = 1 ]; then
        arch=arm64
    fi
    case "$arch" in
        arm64 | aarch64) target=aarch64-apple-darwin ;;
        x86_64 | amd64) target=x86_64-apple-darwin ;;
        *) err "no build for $arch yet; build from source: https://github.com/$REPO" ;;
    esac
}

main() {
    version=${CONSTRUCT_VERSION:-}
    dir=${CONSTRUCT_INSTALL_DIR:-}

    while [ $# -gt 0 ]; do
        case "$1" in
            --version) [ $# -ge 2 ] || err "--version needs a value"; version=$2; shift 2 ;;
            --version=*) version=${1#*=}; shift ;;
            --dir) [ $# -ge 2 ] || err "--dir needs a value"; dir=$2; shift 2 ;;
            --dir=*) dir=${1#*=}; shift ;;
            -h | --help) usage; exit 0 ;;
            *) usage; err "unknown option: $1" ;;
        esac
    done

    if [ -z "$dir" ]; then
        [ -n "${HOME:-}" ] || err "HOME isn't set; pass --dir"
        dir="$HOME/.local/bin"
    fi

    check_platform

    need uname
    need tar
    need mktemp
    need awk
    if command -v curl >/dev/null 2>&1; then
        downloader=curl
    elif command -v wget >/dev/null 2>&1; then
        downloader=wget
    else
        err "this installer needs curl or wget"
    fi

    if [ -z "$version" ]; then
        version=$(latest_version)
    fi
    case "$version" in
        v*) ;;
        *) version="v$version" ;;
    esac

    tmp=$(mktemp -d 2>/dev/null || mktemp -d -t construct-install)
    staged=""
    trap 'rm -rf "$tmp"; [ -z "$staged" ] || rm -f "$staged"' EXIT
    trap 'exit 130' INT TERM

    asset="construct-$target.tar.gz"
    base="https://github.com/$REPO/releases/download/$version"

    say "downloading construct $version ($target)"
    fetch "$base/$asset" "$tmp/$asset" ||
        err "download failed: $base/$asset (does release $version exist?)"
    fetch "$base/$asset.sha256" "$tmp/$asset.sha256" ||
        err "download failed: $base/$asset.sha256"

    expected=$(awk 'NR == 1 {print $1}' "$tmp/$asset.sha256")
    actual=$(sha256 "$tmp/$asset")
    [ -n "$expected" ] && [ "$expected" = "$actual" ] ||
        err "checksum mismatch for $asset (expected $expected, got $actual); nothing was installed"
    say "checksum ok ($actual)"

    tar -xzf "$tmp/$asset" -C "$tmp" || err "couldn't unpack $asset; nothing was installed"
    bin="$tmp/construct-$target/construct"
    [ -f "$bin" ] || err "$asset doesn't contain construct-$target/construct; nothing was installed"

    # Run it before installing: a binary this system can't load (an older
    # glibc on Linux, usually) should fail here, not after replacing a working copy.
    built=$("$bin" --version 2>/dev/null) ||
        err "the downloaded binary doesn't run on this system (on Linux, often a glibc older than the build's); nothing was installed"

    mkdir -p "$dir" || err "couldn't create $dir"
    staged="$dir/.construct.install.$$"
    cp "$bin" "$staged" || err "couldn't write to $dir"
    chmod 0755 "$staged"
    mv -f "$staged" "$dir/construct" || err "couldn't move construct into $dir"
    staged=""

    say "installed $built to $dir/construct"
    case ":${PATH:-}:" in
        *":$dir:"*) ;;
        *)
            say "note: $dir isn't on your PATH. Add this to your shell profile:"
            say "    export PATH=\"$dir:\$PATH\""
            ;;
    esac
    say "run 'construct --help' to start; docs: https://github.com/$REPO#readme"
}

# Everything runs from here, so a download cut off mid-script runs nothing.
main "$@"
