#!/usr/bin/env bash
# run.sh - temporary bootstrap to run firstmate ITSELF inside a podman
# container (not just spawned crewmates), mounting the host's Claude
# credentials and the host's podman socket so this containerized primary
# can spawn sibling crewmate containers beside itself.
#
# Scaffolding for the "everything containerized" phase ahead of the TUI.
# Delete this file and firstmate.Containerfile once the TUI supersedes it.
set -euo pipefail

REPO_ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
IMAGE_NAME="firstmate-primary"
CONTAINER_NAME="firstmate-primary"

if ! command -v podman >/dev/null 2>&1; then
  echo "run.sh: podman not found on host; install it first." >&2
  exit 1
fi

ENV_FILE="$REPO_ROOT/.env"
if [ -f "$ENV_FILE" ] && grep -q '^GH_TOKEN=' "$ENV_FILE" 2>/dev/null; then
  :
else
  if ! command -v gh >/dev/null 2>&1; then
    echo "run.sh: gh not found on host; install it first to auto-provision GH_TOKEN." >&2
    exit 1
  fi
  if ! gh auth status >/dev/null 2>&1; then
    echo "run.sh: no gh auth found; running gh auth login..."
    gh auth login
  fi
  GH_TOKEN_VALUE="$(gh auth token)"
  { grep -v '^GH_TOKEN=' "$ENV_FILE" 2>/dev/null || true; echo "GH_TOKEN=$GH_TOKEN_VALUE"; } > "${ENV_FILE}.tmp"
  mv "${ENV_FILE}.tmp" "$ENV_FILE"
  chmod 600 "$ENV_FILE"
  echo "run.sh: wrote GH_TOKEN to $ENV_FILE"
fi

if [ "$(podman machine list --format '{{.Running}}' 2>/dev/null | head -1)" != "true" ]; then
  echo "run.sh: starting podman machine..."
  podman machine start
fi

echo "run.sh: building ${IMAGE_NAME} (skip with --no-build if already built)..."
if [ "${1:-}" != "--no-build" ]; then
  podman build -t "$IMAGE_NAME" -f "$REPO_ROOT/firstmate.Containerfile" "$REPO_ROOT"
fi

podman rm -f "$CONTAINER_NAME" >/dev/null 2>&1 || true

# The container runs inside the podman-machine VM (applehv), which does
# not share macOS's /var/folders forwarding path into its filesystem, so
# `podman machine inspect`'s PodmanSocket.Path (a macOS-side path) is not
# reachable from inside a container. Use the VM-native socket instead,
# which lives alongside the container in the same VM.
VM_UID="$(podman machine ssh -- id -u 2>/dev/null || true)"
HOST_SOCK=""
if [ -n "$VM_UID" ]; then
  HOST_SOCK="/run/user/${VM_UID}/podman/podman.sock"
fi
if [ -z "$HOST_SOCK" ]; then
  echo "run.sh: could not resolve the podman machine's VM-native socket path; sibling container spawning won't work." >&2
else
  # The socket is owned by the VM's rootless user, which does not match
  # the container's "agent" uid (1000), so the container gets a
  # permission-denied dial without this. The VM is a disposable
  # single-purpose podman-machine, so a world-writable rootless socket
  # there is an acceptable tradeoff for sibling container spawning.
  podman machine ssh -- chmod 666 "$HOST_SOCK" 2>/dev/null || true
fi

# Mount the repo at its own host path (not /work/firstmate) so Claude
# Code's per-path trust decision (~/.claude.json's "projects" map, keyed
# by exact absolute path) matches the already-trusted host entry instead
# of looking like an unseen directory.
MOUNT_ARGS=(
  -v "$HOME/.claude:/home/agent/.claude"
  -v "$HOME/.claude.json:/home/agent/.claude.json"
  -v "$REPO_ROOT:$REPO_ROOT"
)
if [ -n "$HOST_SOCK" ]; then
  MOUNT_ARGS+=(-v "$HOST_SOCK:/run/podman/podman.sock")
fi
if [ -f "$ENV_FILE" ]; then
  MOUNT_ARGS+=(--env-file "$ENV_FILE")
fi

echo "run.sh: launching ${CONTAINER_NAME}..."
exec podman run -it --init --name "$CONTAINER_NAME" \
  "${MOUNT_ARGS[@]}" \
  -e CONTAINER_HOST=unix:///run/podman/podman.sock \
  -w "$REPO_ROOT" \
  "$IMAGE_NAME" \
  claude --permission-mode auto
