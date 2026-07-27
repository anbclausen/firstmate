#!/usr/bin/env bash
# install.sh - compiles and installs the firstmate TUI (`fm`).
#
# podman is the ONLY host prerequisite this script, and the fm binary it
# installs, ever require - no host Rust toolchain, no other tool. Compiling
# tui/ happens entirely inside tui/Containerfile's own build container (see
# that file's header for the environment it provides); this script never
# runs cargo on the host.
#
# The installed `fm` is the host-side launcher shell script (tui/fm): on every
# run it execs `podman run` into tui/runtime.Containerfile's container, the
# same podman-privileged role the retired root run.sh/firstmate.Containerfile
# used to play for the firstmate primary. The compiled Rust binary is Linux-
# only and runs only inside that container - this script produces it, places
# the launcher, and never runs the TUI itself.
set -euo pipefail

REPO_ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
BUILD_IMAGE="fm-tui-build"
BUILD_CONTAINER="fm-tui-build-tmp"
BIN_DIR="${FM_INSTALL_BIN_DIR:-$HOME/.local/bin}"

ensure_podman() {
  if command -v podman >/dev/null 2>&1; then
    return
  fi
  echo "install.sh: podman not found; installing..."
  case "$(uname)" in
    Darwin)
      if ! command -v brew >/dev/null 2>&1; then
        echo "install.sh: Homebrew not found; install podman manually: https://podman.io/docs/installation" >&2
        exit 1
      fi
      brew install podman
      ;;
    Linux)
      if command -v apt-get >/dev/null 2>&1; then
        sudo apt-get update && sudo apt-get install -y podman
      elif command -v dnf >/dev/null 2>&1; then
        sudo dnf install -y podman
      else
        echo "install.sh: no supported package manager found (apt-get/dnf); install podman manually: https://podman.io/docs/installation" >&2
        exit 1
      fi
      ;;
    *)
      echo "install.sh: unsupported OS $(uname); install podman manually: https://podman.io/docs/installation" >&2
      exit 1
      ;;
  esac
}

# podman on macOS runs containers inside a Linux VM ("podman machine") that
# must exist and be running before any build works; Linux talks to the local
# kernel directly and needs no machine.
ensure_podman_machine() {
  [ "$(uname)" = Darwin ] || return 0
  if ! podman machine list --format '{{.Name}}' 2>/dev/null | grep -q .; then
    echo "install.sh: initializing podman machine..."
    podman machine init
  fi
  if [ "$(podman machine list --format '{{.Running}}' 2>/dev/null | head -1)" != "true" ]; then
    echo "install.sh: starting podman machine..."
    podman machine start
  fi
}

# macOS podman machines run in a Linux VM whose clock drifts behind real time
# after the host sleeps; a VM clock in the past makes apt reject repos whose
# metadata looks future-dated (and can break TLS). Step it back to the host's
# wall clock. No-op on Linux, where containers share the host kernel's clock.
sync_podman_clock() {
  [ "$(uname)" = Darwin ] || return 0
  podman machine ssh "sudo date -s @$(date -u +%s)" >/dev/null 2>&1 \
    || echo "install.sh: warning: could not resync podman machine clock" >&2
}

ensure_podman
ensure_podman_machine
sync_podman_clock

if [ -x "$BIN_DIR/fm" ] && [ -t 0 ] && [ -z "${FM_FORCE_REINSTALL:-}" ]; then
  printf 'install.sh: fm already installed at %s. Reinstall? [y/N] ' "$BIN_DIR/fm"
  read -r reply
  case "$reply" in
    y | Y | yes | YES) ;;
    *) echo "install.sh: keeping existing fm; nothing to do."; exit 0 ;;
  esac
fi

echo "install.sh: building the tui build image..."
podman build -t "$BUILD_IMAGE" -f "$REPO_ROOT/tui/Containerfile" "$REPO_ROOT/tui"

echo "install.sh: compiling fm inside the container (host needs no Rust toolchain)..."
podman rm -f "$BUILD_CONTAINER" >/dev/null 2>&1 || true
# The source mount is read-only and every write (registry cache, build
# output) goes to named volumes instead, so the build never depends on the
# bind-mounted tui/ directory's host-side ownership matching the image's
# non-root "agent" user. ":U" has podman chown each volume's content to that
# container user on start, since a freshly created named volume defaults to
# root-owned otherwise.
podman run --name "$BUILD_CONTAINER" \
  -v "$REPO_ROOT/tui:$REPO_ROOT/tui:ro" \
  -v fm-tui-cargo-registry:/home/agent/.cargo/registry:U \
  -v fm-tui-cargo-target:/home/agent/target:U \
  -e CARGO_TARGET_DIR=/home/agent/target \
  -w "$REPO_ROOT/tui" \
  "$BUILD_IMAGE" \
  cargo build --release --locked

mkdir -p "$REPO_ROOT/tui/target/release"
podman cp "$BUILD_CONTAINER:/home/agent/target/release/firstmate-tui" "$REPO_ROOT/tui/target/release/firstmate-tui"
podman rm -f "$BUILD_CONTAINER" >/dev/null 2>&1 || true
chmod +x "$REPO_ROOT/tui/target/release/firstmate-tui"

# fm is the host-side launcher (tui/fm): a native shell script that execs
# `podman run` into the runtime container, where the Linux binary above and
# firstmate's whole toolchain live. The compiled binary is Linux-only and
# never runs on the host directly; installing it as `fm` on macOS would just
# yield "exec format error". The repo path is baked in because the binary is
# bind-mounted from this fixed location on every launch.
mkdir -p "$BIN_DIR"
sed "s|__FM_REPO_ROOT__|$REPO_ROOT|" "$REPO_ROOT/tui/fm" > "$BIN_DIR/fm"
chmod +x "$BIN_DIR/fm"

echo "install.sh: installed fm -> $BIN_DIR/fm"
case ":$PATH:" in
  *":$BIN_DIR:"*) ;;
  *) echo "install.sh: add $BIN_DIR to your PATH, e.g.: export PATH=\"$BIN_DIR:\$PATH\"" ;;
esac
