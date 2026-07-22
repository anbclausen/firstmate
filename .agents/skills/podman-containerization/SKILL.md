---
name: podman-containerization
description: >-
  Agent-only container-selection checklist for Firstmate's experimental podman
  runtime backend. Use before spawning a podman-backed crewmate: checks the
  target project for its own Containerfile, and otherwise picks the best-fit
  standard profile (dev or scout) from containers/, explaining the
  least-privilege rationale.
user-invocable: false
metadata:
  internal: true
---

# podman-containerization

Load this before spawning any task with `--backend podman` / `FM_BACKEND=podman`
/ `config/backend` set to `podman`, alongside `harness-adapters` (AGENTS.md
section 4/13 - harness and runtime backend are separate axes; this skill owns
only the container axis).

It does not replace `AGENTS.md`, `docs/podman-backend.md`, or
`harness-adapters`. Implementation details, the label/garbage-collection
contract, and open questions live in `docs/podman-backend.md`; this is the
operator checklist for the one decision this backend adds to an ordinary
spawn: which container image the task runs in.

## Preflight

Confirm podman is intentionally selected (never auto-detected - see
`docs/podman-backend.md`).
Confirm `podman info` succeeds before expecting spawn to work; a missing
binary or unreachable machine/service is a blocker, not a fallback to tmux.

## Container selection (the decision this skill owns)

Before calling `bin/fm-spawn.sh --backend podman`, resolve which image the
task's container will run from - `fm_backend_podman_image_for` in
`bin/backends/podman.sh` performs this same resolution mechanically, but
explain the reasoning to the captain when it is not obvious:

1. **Check the target project's repo root for its own `Containerfile`.**
   If present, that image is built and used - the project has opted into a
   custom environment (its own toolchain, extra system packages, a specific
   base image), so the standard profiles are skipped entirely. This is a
   devcontainer-style override, not a merge: the project's Containerfile is
   the whole image definition.
2. **Otherwise, pick the best-fit standard profile from `containers/`** by
   the task's classified deliverable (AGENTS.md section 7):
   - **scout** (investigation, diagnosis, planning, reproduction, audit -
     never expected to write code) -> `containers/scout.Containerfile`.
     Runs with a read-only root filesystem, the project mounted read-only,
     no network (`--network=none`), and only a small tmpfs scratch
     directory writable. A scout task genuinely does not need to modify the
     project or reach the network to investigate and report, so those
     capabilities are withheld rather than granted-then-trusted-not-to-use.
   - **ship** (the default; implementation work) -> `containers/dev.Containerfile`.
     Runs with the project mounted read-write and normal network access
     (registries, `gh`, `git fetch`/`push`), since a coding task genuinely
     needs both.

Explain the least-privilege rationale in plain terms when it matters to the
captain: the container never gets `--privileged`, never gets added Linux
capabilities beyond the image default (`--cap-drop=ALL`), and never sees any
host path beyond the one project it is working on - a scout additionally
cannot write outside its scratch directory or reach the network at all. This
is the concrete meaning of "sandboxed" for this backend; point to
`docs/podman-backend.md` "Container profiles" for the exact flag set rather
than restating it.

## When the classification is ambiguous

If a task's deliverable classification itself is unclear (ship vs scout),
resolve that under AGENTS.md section 7's ordinary intake rules FIRST - this
skill only maps an already-classified task onto a profile, it does not
reclassify the task. If a scout task later needs network access it turns out
it did not anticipate (e.g. to fetch a URL for the investigation), that is a
scope question for the captain, not something to silently work around by
switching the container's flags.

## After spawn

Supervise a podman-backed task exactly like any other crewmate through the
ordinary `bin/fm-peek.sh` / `bin/fm-send.sh` / `bin/fm-crew-state.sh` /
`bin/fm-teardown.sh` surface - the backend abstraction means no
podman-specific supervision commands are needed. Teardown stops and removes
the task's own container automatically; images are shared/cached across
tasks and are reclaimed only by the periodic garbage-collection sweep
(`docs/podman-backend.md` "Garbage collection"), never at individual task
teardown.
