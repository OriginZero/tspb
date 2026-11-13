#!/bin/sh
# tspb installer for Linux and macOS.
#
#   curl -fsSL https://raw.githubusercontent.com/OriginZero/tspb/main/install.sh | sh
#
# Environment variables:
#   TSPB_INSTALL_DIR  where to put the binary (default: $HOME/.local/bin)
#   TSPB_VERSION      release to install, e.g. v0.2.0 (default: latest)
#   TSPB_MIRROR       GitHub mirror prefix, e.g. https://ghfast.top/
#
# Without TSPB_MIRROR the direct download is tried first, then public GitHub
# mirrors are used as fallbacks, so this works from slow/blocked networks too.
set -eu

BIN="tspb"
REPO="OriginZero/tspb"
INSTALL_DIR="${TSPB_INSTALL_DIR:-$HOME/.local/bin}"
VERSION="${TSPB_VERSION:-}"
MIRROR="${TSPB_MIRROR:-}"
MIRRORS="https://ghfast.top/ https://gh-proxy.com/ https://ghproxy.net/ https://hub.gitmirror.com/"
API="https://api.github.com/repos/${REPO}/releases/latest"

main() {
	echo ""
	echo "  tspb installer"
	echo ""

	detect_platform
	need curl
	need tar

	if [ -z "$VERSION" ]; then
		log "resolving the latest release..."
		VERSION="$(latest_version || true)"
		[ -n "$VERSION" ] || err "cannot reach GitHub; retry with TSPB_VERSION=vX.Y.Z or TSPB_MIRROR=https://your-mirror/"
	fi
	case "$VERSION" in
		v*) ;;
		*) VERSION="v$VERSION" ;;
	esac

	ASSET="${BIN}-${VERSION}-${TARGET}.tar.gz"
	BASE="https://github.com/${REPO}/releases/download/${VERSION}"
	log "installing ${VERSION} for ${TARGET}"

	TMP="$(mktemp -d)"
	trap 'rm -rf "$TMP"' EXIT

	fetch "${BASE}/${ASSET}" "${TMP}/${ASSET}" || err "download failed: ${BASE}/${ASSET}"

	EXPECTED="$(sha256_expected "${BASE}/SHA256SUMS.txt" "$ASSET")"
	if [ -n "$EXPECTED" ]; then
		verify_sha256 "${TMP}/${ASSET}" "$EXPECTED" || err "checksum mismatch, nothing was installed"
		log "checksum verified"
	else
		warn "no SHA256SUMS.txt for this release, skipping checksum verification"
	fi

	tar -xzf "${TMP}/${ASSET}" -C "$TMP"
	[ -f "${TMP}/${BIN}" ] || err "archive does not contain ${BIN}"

	mkdir -p "$INSTALL_DIR"
	cp "${TMP}/${BIN}" "${INSTALL_DIR}/${BIN}"
	chmod +x "${INSTALL_DIR}/${BIN}"

	log "installed ${INSTALL_DIR}/${BIN}"

	case ":${PATH}:" in
		*":${INSTALL_DIR}:"*) ;;
		*)
			echo ""
			warn "${INSTALL_DIR} is not in your PATH. Add it to your shell config:"
			echo ""
			echo "  export PATH=\"${INSTALL_DIR}:\$PATH\""
			echo ""
			;;
	esac

	echo ""
	log "ready. run 'tspb' inside a Spring Boot repository, 'tspb update' to upgrade."
	echo ""
}

detect_platform() {
	OS="$(uname -s)"
	case "$OS" in
		Linux) os="linux" ;;
		Darwin) os="macos" ;;
		*) err "unsupported OS: $OS" ;;
	esac

	ARCH="$(uname -m)"
	case "$ARCH" in
		x86_64|amd64) arch="x86_64" ;;
		aarch64|arm64) arch="aarch64" ;;
		*) err "unsupported architecture: $ARCH" ;;
	esac

	case "${os}-${arch}" in
		linux-x86_64) TARGET="x86_64-unknown-linux-gnu" ;;
		linux-aarch64) TARGET="aarch64-unknown-linux-gnu" ;;
		macos-x86_64) TARGET="x86_64-apple-darwin" ;;
		macos-aarch64) TARGET="aarch64-apple-darwin" ;;
		*) err "no prebuilt binary for ${os}/${arch}" ;;
	esac
	log "detected ${os}/${arch} (${TARGET})"
}

latest_version() {
	body="$(curl -fsSL --retry 3 --connect-timeout 10 --max-time 30 "$API" 2>/dev/null)" || return 1
	printf '%s' "$body" | tr -d '\n' |
		sed -n 's/.*"tag_name"[[:space:]]*:[[:space:]]*"\([^"]*\)".*/\1/p' | head -n 1
}

# fetch URL OUT [TIMEOUT] — pinned mirror first when set, otherwise direct then mirrors.
fetch() {
	_url="$1"
	_out="$2"
	_max="${3:-300}"
	if [ -n "$MIRROR" ]; then
		curl -fsSL --retry 2 --connect-timeout 10 --max-time "$_max" "${MIRROR}${_url}" -o "$_out" && return 0
		return 1
	fi
	curl -fsSL --retry 2 --connect-timeout 10 --max-time "$_max" "$_url" -o "$_out" && return 0
	for _m in $MIRRORS; do
		log "retrying via ${_m}"
		curl -fsSL --retry 1 --connect-timeout 10 --max-time "$_max" "${_m}${_url}" -o "$_out" && return 0
	done
	return 1
}

sha256_expected() {
	_sums="$(mktemp)"
	if fetch "$1" "$_sums" 30 2>/dev/null; then
		awk -v asset="$2" '$2 == asset { print $1; exit }' "$_sums"
	fi
	rm -f "$_sums"
}

verify_sha256() {
	if command -v sha256sum >/dev/null 2>&1; then
		_actual="$(sha256sum "$1" | awk '{ print $1 }')"
	elif command -v shasum >/dev/null 2>&1; then
		_actual="$(shasum -a 256 "$1" | awk '{ print $1 }')"
	elif command -v openssl >/dev/null 2>&1; then
		_actual="$(openssl dgst -sha256 "$1" | awk '{ print $NF }')"
	else
		warn "no sha256sum, shasum or openssl found; skipping checksum verification"
		return 0
	fi
	[ "$(printf '%s' "$_actual" | tr 'A-F' 'a-f')" = "$(printf '%s' "$2" | tr 'A-F' 'a-f')" ]
}

log() { printf ' \033[32m>\033[0m %s\n' "$1"; }
warn() { printf ' \033[33m!\033[0m %s\n' "$1"; }
err() {
	printf ' \033[31mx\033[0m %s\n' "$1" >&2
	exit 1
}

need() {
	command -v "$1" >/dev/null 2>&1 || err "requires '$1'; install it or download a release manually"
}

main "$@"
