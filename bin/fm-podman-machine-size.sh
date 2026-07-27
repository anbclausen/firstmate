#!/usr/bin/env bash
# fm-podman-machine-size.sh - size the macOS podman machine before firstmate
# or the TUI runs any container in it.
#
# Policy: RECOMMEND half the host's RAM and CPUs, with a floor of 8 GB and 2
# CPUs, and only ever RAISE - never shrink a machine the captain sized larger.
# These are defaults the user must agree to: a needed bump prompts for consent
# when attached to a terminal, and requires FM_PODMAN_SIZE_ASSUME_YES=1 when
# non-interactive, because applying it restarts the machine and claims host
# resources.
# Rationale: a podman crewmate runs its own agent, and no-mistakes spawns a
# second nested agent per pipeline step, so an undersized machine OOM-kills
# mid-run (see data/learnings.md 2026-07-26). Override the floor with
# FM_PODMAN_MEMORY_MIN_MB / FM_PODMAN_CPUS_MIN.
#
# macOS only: native Linux podman has no machine (containers share the host
# kernel directly), so this is a no-op there. Applying a memory/CPU change
# requires stopping the machine, so this restarts it when a bump is needed;
# call it before launching any long-lived container, never underneath one.
set -euo pipefail

[ "$(uname)" = Darwin ] || exit 0

MEM_MIN_MB=${FM_PODMAN_MEMORY_MIN_MB:-8192}
CPUS_MIN=${FM_PODMAN_CPUS_MIN:-2}

command -v podman >/dev/null 2>&1 || { echo "fm-podman-machine-size: podman not found" >&2; exit 0; }

# Machine must already exist (install.sh / run.sh init it first). If it does
# not, skip quietly rather than second-guessing their init flow.
podman machine list --format '{{.Name}}' 2>/dev/null | grep -q . || exit 0

host_mem_mb=$(( $(sysctl -n hw.memsize) / 1048576 ))
host_cpus=$(sysctl -n hw.ncpu)

target_mem=$(( host_mem_mb / 2 ))
[ "$target_mem" -lt "$MEM_MIN_MB" ] && target_mem=$MEM_MIN_MB
# Never ask for more RAM than the host physically has.
[ "$target_mem" -gt "$host_mem_mb" ] && target_mem=$host_mem_mb

target_cpus=$(( host_cpus / 2 ))
[ "$target_cpus" -lt "$CPUS_MIN" ] && target_cpus=$CPUS_MIN
[ "$target_cpus" -gt "$host_cpus" ] && target_cpus=$host_cpus

cur_mem=$(podman machine inspect --format '{{.Resources.Memory}}' 2>/dev/null || echo 0)
cur_cpus=$(podman machine inspect --format '{{.Resources.CPUs}}' 2>/dev/null || echo 0)
case "$cur_mem$cur_cpus" in *[!0-9]*|'') echo "fm-podman-machine-size: could not read current machine size; leaving it unchanged" >&2; exit 0;; esac

# Raise-only: leave a machine that already meets both targets alone.
if [ "$cur_mem" -ge "$target_mem" ] && [ "$cur_cpus" -ge "$target_cpus" ]; then
  exit 0
fi

new_mem=$cur_mem; [ "$cur_mem" -lt "$target_mem" ] && new_mem=$target_mem
new_cpus=$cur_cpus; [ "$cur_cpus" -lt "$target_cpus" ] && new_cpus=$target_cpus

# Consent: resizing restarts the machine and claims host RAM/CPUs, so the
# recommended values above are only DEFAULTS the user must agree to. Prompt
# when attached to a terminal; when non-interactive (e.g. curl | sh), require
# explicit FM_PODMAN_SIZE_ASSUME_YES=1 rather than restarting silently.
echo "fm-podman-machine-size: podman machine is ${cur_mem} MB / ${cur_cpus} CPUs; recommended is ${new_mem} MB / ${new_cpus} CPUs (half the host, floor ${MEM_MIN_MB} MB / ${CPUS_MIN} CPUs)."
if [ "${FM_PODMAN_SIZE_ASSUME_YES:-}" = 1 ]; then
  echo "fm-podman-machine-size: FM_PODMAN_SIZE_ASSUME_YES=1; applying."
elif [ -t 0 ]; then
  printf 'fm-podman-machine-size: apply this? it will restart the podman machine [Y/n] '
  read -r reply
  case "$reply" in
    n | N | no | NO) echo "fm-podman-machine-size: leaving the machine unchanged."; exit 0 ;;
    *) ;;
  esac
else
  echo "fm-podman-machine-size: non-interactive; not resizing without consent. Re-run in a terminal, or set FM_PODMAN_SIZE_ASSUME_YES=1 (or size it yourself with 'podman machine set --memory <MB> --cpus <n>')." >&2
  exit 0
fi

echo "fm-podman-machine-size: raising podman machine to ${new_mem} MB / ${new_cpus} CPUs; this restarts the machine..."
was_running=false
[ "$(podman machine list --format '{{.Running}}' 2>/dev/null | head -1)" = "true" ] && was_running=true

if $was_running; then podman machine stop; fi
podman machine set --memory "$new_mem" --cpus "$new_cpus"
# Restart if it was running before, so callers find it running as they expect.
if $was_running; then podman machine start; fi
