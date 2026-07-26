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
        let cmd = match build_override {
            Some(s) => split_command_line(&s)?,
            None => default_build_command(root),
        };
        println!("firstmate TUI: building the runtime image (cached after first launch)...");
        run_build_or_crash(&cmd)?;
    }

    exec_runtime(root)
}

/// Returns the full `podman build ...` argv (program included) as separate
/// words, rather than one shell string, so a repo path containing a space
/// isn't mis-split later by whitespace parsing.
fn default_build_command(root: &Path) -> Vec<String> {
    vec![
        "podman".to_string(),
        "build".to_string(),
        "-t".to_string(),
        RUNTIME_IMAGE.to_string(),
        "-f".to_string(),
        format!("{}/tui/runtime.Containerfile", root.display()),
        root.display().to_string(),
    ]
}

/// Splits an `FM_TUI_PODMAN_BUILD` override string into argv words,
/// honoring single/double quotes (and backslash escapes outside single
/// quotes) so a quoted path containing a space survives as one argument -
/// plain `split_whitespace()` would mis-split it, the same bug the default
/// build command was fixed against.
fn split_command_line(s: &str) -> anyhow::Result<Vec<String>> {
    let mut words = Vec::new();
    let mut current = String::new();
    let mut in_word = false;
    let mut chars = s.chars().peekable();
    while let Some(c) = chars.next() {
        match c {
            c if c.is_whitespace() => {
                if in_word {
                    words.push(std::mem::take(&mut current));
                    in_word = false;
                }
            }
            '\'' => {
                in_word = true;
                for c in chars.by_ref() {
                    if c == '\'' {
                        break;
                    }
                    current.push(c);
                }
            }
            '"' => {
                in_word = true;
                loop {
                    match chars.next() {
                        Some('"') | None => break,
                        Some('\\') if matches!(chars.peek(), Some('"') | Some('\\')) => {
                            current.push(chars.next().unwrap());
                        }
                        Some(c) => current.push(c),
                    }
                }
            }
            '\\' => {
                in_word = true;
                if let Some(next) = chars.next() {
                    current.push(next);
                }
            }
            c => {
                in_word = true;
                current.push(c);
            }
        }
    }
    if in_word {
        words.push(current);
    }
    if words.is_empty() {
        anyhow::bail!("FM_TUI_PODMAN_BUILD is set but empty");
    }
    Ok(words)
}

fn run_build_or_crash(build_cmd: &[String]) -> anyhow::Result<()> {
    let program = build_cmd
        .first()
        .ok_or_else(|| anyhow::anyhow!("empty build command"))?;
    let args: Vec<&str> = build_cmd[1..].iter().map(String::as_str).collect();
    let outcome = loading::run_build_command(program, &args)?;
    if !outcome.success {
        loading::crash_with_build_log(&outcome.log);
    }
    Ok(())
}

fn container_is_running(name: &str) -> bool {
    Command::new("podman")
        .args([
            "ps",
            "--filter",
            &format!("name=^{name}$"),
            "--filter",
            "status=running",
            "--format",
            "{{.Names}}",
        ])
        .output()
        .map(|o| !o.stdout.is_empty())
        .unwrap_or(false)
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
/// Returns an error rather than degrading silently, since a container
/// launched without this mount can't see sibling crewmate containers at all.
fn podman_socket_path() -> anyhow::Result<String> {
    let output = Command::new("podman")
        .args(["info", "--format", "{{.Host.RemoteSocket.Path}}"])
        .output()
        .map_err(|e| anyhow::anyhow!("failed to run `podman info` to resolve the podman socket path: {e}"))?;
    if !output.status.success() {
        anyhow::bail!(
            "`podman info` failed while resolving the podman socket path; \
             refusing to relaunch without sibling-container visibility"
        );
    }
    let path = String::from_utf8(output.stdout)
        .map_err(|e| anyhow::anyhow!("`podman info` output was not valid UTF-8: {e}"))?
        .trim()
        .to_string();
    if path.is_empty() {
        anyhow::bail!(
            "podman reported an empty socket path; \
             refusing to relaunch without sibling-container visibility"
        );
    }
    Ok(path)
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

    // The fixed container name lets a stale container from a crashed prior
    // run collide with a fresh launch, but a *running* one means another
    // session is active - refuse instead of silently killing it out from
    // under that session.
    if container_is_running(CONTAINER_NAME) {
        anyhow::bail!(
            "a firstmate TUI container (\"{CONTAINER_NAME}\") is already running; \
             refusing to take over another active session. Stop it first with \
             `podman rm -f {CONTAINER_NAME}` if it's actually stale."
        );
    }
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

    let sock = podman_socket_path()?;
    cmd.arg("-v")
        .arg(format!("{sock}:/run/podman/podman.sock"))
        .args(["-e", "CONTAINER_HOST=unix:///run/podman/podman.sock"]);

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
            vec![
                "podman",
                "build",
                "-t",
                "fm-tui-runtime",
                "-f",
                "/repo/tui/runtime.Containerfile",
                "/repo",
            ]
        );
    }

    #[test]
    fn default_build_command_keeps_a_spaced_repo_path_as_one_argument() {
        let cmd = default_build_command(Path::new("/repo with space"));
        assert_eq!(cmd.last().unwrap(), "/repo with space");
        assert_eq!(cmd.len(), 7);
    }

    #[test]
    fn split_command_line_keeps_a_quoted_spaced_path_as_one_argument() {
        let cmd = split_command_line("podman build -t img -f 'Containerfile' \"/repo with space\"").unwrap();
        assert_eq!(cmd, vec!["podman", "build", "-t", "img", "-f", "Containerfile", "/repo with space"]);
    }

    #[test]
    fn split_command_line_rejects_empty_override() {
        assert!(split_command_line("   ").is_err());
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
