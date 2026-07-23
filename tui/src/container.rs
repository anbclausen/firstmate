//! Outer host-side relaunch, run only when `CONTAINERIZED_MARKER` is unset:
//! ensures the runtime image is built, then execs `podman run` so the TUI's
//! real process lives inside podman with the same privileges the firstmate
//! primary itself needs (podman-socket access to see sibling crewmate
//! containers) - modeled directly on the retired root run.sh and
//! firstmate.Containerfile, which this supersedes. Once the marker is set
//! (we're already inside that container), `main` skips this module entirely.

use std::env;
use std::os::unix::process::CommandExt;
use std::path::Path;
use std::process::Command;

use crate::loading;

pub const CONTAINERIZED_MARKER: &str = "FM_TUI_CONTAINERIZED";

const RUNTIME_IMAGE: &str = "fm-tui-runtime";
const CONTAINER_NAME: &str = "fm-tui";
const TREEHOUSE_VOLUME: &str = "fm-tui-treehouse";
const NO_MISTAKES_VOLUME: &str = "fm-tui-no-mistakes";

/// Ensures the runtime image exists and execs into it. Only returns on
/// failure (image build failed, binary missing, or the exec syscall itself
/// failed) - a successful relaunch never returns, because `exec` replaces
/// this process with `podman run`.
pub fn relaunch_into_runtime(root: &Path) -> anyhow::Result<()> {
    ensure_podman_machine_running();

    // FM_TUI_PODMAN_BUILD lets a caller force a specific build/pull command
    // (loading screen and crash-on-failure logging still apply); absent,
    // we build the runtime image ourselves whenever it isn't already cached.
    let build_override = env::var("FM_TUI_PODMAN_BUILD").ok();
    if build_override.is_some() || !image_exists(RUNTIME_IMAGE) {
        let cmd = build_override.unwrap_or_else(|| default_build_command(root));
        println!("firstmate TUI: building the runtime image (cached after first launch)...");
        run_build_or_crash(&cmd)?;
    }

    exec_runtime(root)
}

fn default_build_command(root: &Path) -> String {
    format!(
        "podman build -t {RUNTIME_IMAGE} -f {}/tui/runtime.Containerfile {}",
        root.display(),
        root.display(),
    )
}

fn run_build_or_crash(build_cmd: &str) -> anyhow::Result<()> {
    let mut parts = build_cmd.split_whitespace();
    let program = parts
        .next()
        .ok_or_else(|| anyhow::anyhow!("empty build command"))?;
    let args: Vec<&str> = parts.collect();
    let outcome = loading::run_build_command(program, &args)?;
    if !outcome.success {
        loading::crash_with_build_log(&outcome.log);
    }
    Ok(())
}

fn image_exists(tag: &str) -> bool {
    Command::new("podman")
        .args(["image", "exists", tag])
        .status()
        .map(|s| s.success())
        .unwrap_or(false)
}

/// macOS-only: podman there runs inside a Linux VM ("podman machine") that
/// must be initialized/running before any `podman build`/`run` call works.
/// Linux talks to the local kernel directly and needs no machine.
fn ensure_podman_machine_running() {
    if env::consts::OS != "macos" {
        return;
    }
    let has_machine = Command::new("podman")
        .args(["machine", "list", "--format", "{{.Name}}"])
        .output()
        .map(|o| !o.stdout.is_empty())
        .unwrap_or(false);
    if !has_machine {
        println!("firstmate TUI: initializing podman machine...");
        let _ = Command::new("podman").args(["machine", "init"]).status();
    }
    let running = Command::new("podman")
        .args(["machine", "list", "--format", "{{.Running}}"])
        .output()
        .ok()
        .and_then(|o| String::from_utf8(o.stdout).ok())
        .map(|s| s.lines().next().unwrap_or("").trim() == "true")
        .unwrap_or(false);
    if !running {
        println!("firstmate TUI: starting podman machine...");
        let _ = Command::new("podman").args(["machine", "start"]).status();
    }
}

/// Resolves the running podman daemon's own control socket path. Valid as a
/// bind-mount source on both a native Linux daemon and inside a macOS podman
/// machine VM, since podman reports whichever filesystem the daemon itself
/// sees - unlike hand-constructing the path, this needs no VM-vs-native
/// branch (see the retired run.sh for the VM-ssh approach this replaces).
fn podman_socket_path() -> Option<String> {
    let output = Command::new("podman")
        .args(["info", "--format", "{{.Host.RemoteSocket.Path}}"])
        .output()
        .ok()?;
    if !output.status.success() {
        return None;
    }
    let path = String::from_utf8(output.stdout).ok()?.trim().to_string();
    if path.is_empty() {
        None
    } else {
        Some(path)
    }
}

fn exec_runtime(root: &Path) -> anyhow::Result<()> {
    let root_str = root.to_string_lossy().to_string();
    let binary = root.join("tui/target/release/firstmate-tui");
    if !binary.is_file() {
        anyhow::bail!(
            "compiled fm binary not found at {} - run ./install.sh again",
            binary.display()
        );
    }

    // A stale container from a crashed prior run would otherwise collide on
    // the fixed name below.
    let _ = Command::new("podman")
        .args(["rm", "-f", CONTAINER_NAME])
        .output();

    let mut cmd = Command::new("podman");
    cmd.args(["run", "-it", "--init", "--name", CONTAINER_NAME])
        // Same privilege pair the retired run.sh used: root bypasses the
        // DAC uid/gid check on the mounted podman socket entirely (podman's
        // own maintainers confirm socket access amounts to a full container
        // escape regardless of which mechanism grants it - containers/podman
        // discussion #24302), and label=disable drops the SELinux
        // confinement that separately blocks access. Acceptable here because
        // this container already gets the repo and Claude credentials and
        // orchestrates sibling crewmate containers, exactly like the
        // firstmate primary it replaces; crewmate containers it spawns
        // never inherit any of this.
        .args(["--user", "0:0"])
        .args(["--security-opt", "label=disable"])
        .arg("-v")
        .arg(format!("{root_str}:{root_str}"))
        .arg("-v")
        .arg(format!("{TREEHOUSE_VOLUME}:/home/agent/.treehouse"))
        .arg("-v")
        .arg(format!("{NO_MISTAKES_VOLUME}:/home/agent/.no-mistakes"))
        .arg("-e")
        .arg(format!("{CONTAINERIZED_MARKER}=1"))
        .args(["-e", "HOME=/home/agent"])
        .arg("-w")
        .arg(&root_str);

    if let Some(sock) = podman_socket_path() {
        cmd.arg("-v")
            .arg(format!("{sock}:/run/podman/podman.sock"))
            .args(["-e", "CONTAINER_HOST=unix:///run/podman/podman.sock"]);
    } else {
        eprintln!(
            "firstmate TUI: warning: could not resolve the podman socket; \
             sibling crewmate containers won't be visible from inside."
        );
    }

    if let Some(home) = env::var_os("HOME") {
        let home = Path::new(&home);
        let claude_dir = home.join(".claude");
        let claude_json = home.join(".claude.json");
        if claude_dir.is_dir() {
            cmd.arg("-v")
                .arg(format!("{}:/home/agent/.claude", claude_dir.display()));
        }
        if claude_json.is_file() {
            cmd.arg("-v").arg(format!(
                "{}:/home/agent/.claude.json",
                claude_json.display()
            ));
        }
    }

    // Forwarded only if the captain already created one (e.g. holding
    // GH_TOKEN) - install.sh never asks the host for gh credentials itself,
    // since podman is meant to be the only host-side prerequisite; gh is
    // available inside the container for a first `gh auth login` instead.
    let env_file = root.join(".env");
    if env_file.is_file() {
        cmd.arg("--env-file").arg(env_file.display().to_string());
    }

    cmd.arg(RUNTIME_IMAGE);
    cmd.arg(binary.display().to_string());

    let err = cmd.exec();
    Err(anyhow::anyhow!("failed to exec podman run: {err}"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_build_command_names_the_runtime_image_and_containerfile() {
        let cmd = default_build_command(Path::new("/repo"));
        assert_eq!(
            cmd,
            "podman build -t fm-tui-runtime -f /repo/tui/runtime.Containerfile /repo"
        );
    }

    #[test]
    fn ensure_podman_machine_running_is_a_no_op_off_macos() {
        // Guards against ever shelling out to `podman machine` on Linux,
        // where no such subcommand exists; only meaningful on non-macOS CI.
        if env::consts::OS != "macos" {
            ensure_podman_machine_running();
        }
    }
}
