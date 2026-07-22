use std::fmt;
use std::fs;
use std::io::Write as _;
use std::path::{Path, PathBuf};

/// The verified firstmate harnesses this TUI can wrap.
/// Kept in sync by hand with AGENTS.md section 4's verified-harness list.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Harness {
    Claude,
    Codex,
    Opencode,
    Pi,
    Grok,
}

impl Harness {
    pub const ALL: [Harness; 5] = [
        Harness::Claude,
        Harness::Codex,
        Harness::Opencode,
        Harness::Pi,
        Harness::Grok,
    ];

    pub fn command(self) -> &'static str {
        match self {
            Harness::Claude => "claude",
            Harness::Codex => "codex",
            Harness::Opencode => "opencode",
            Harness::Pi => "pi",
            Harness::Grok => "grok",
        }
    }

    fn parse(s: &str) -> Option<Harness> {
        match s.trim() {
            "claude" => Some(Harness::Claude),
            "codex" => Some(Harness::Codex),
            "opencode" => Some(Harness::Opencode),
            "pi" => Some(Harness::Pi),
            "grok" => Some(Harness::Grok),
            _ => None,
        }
    }
}

impl fmt::Display for Harness {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.command())
    }
}

/// The path this repo's `config/*` convention reserves for the TUI's chosen
/// default harness, local and gitignored like its siblings (AGENTS.md section 2).
pub fn config_path(repo_root: &Path) -> PathBuf {
    repo_root.join("config").join("tui-harness")
}

pub fn load_default_harness(repo_root: &Path) -> Option<Harness> {
    let contents = fs::read_to_string(config_path(repo_root)).ok()?;
    Harness::parse(&contents)
}

pub fn save_default_harness(repo_root: &Path, harness: Harness) -> anyhow::Result<()> {
    let path = config_path(repo_root);
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    let mut file = fs::File::create(path)?;
    writeln!(file, "{}", harness.command())?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn round_trips_through_disk() {
        let dir = tempdir("round-trips");
        assert!(load_default_harness(&dir).is_none());
        save_default_harness(&dir, Harness::Grok).unwrap();
        assert_eq!(load_default_harness(&dir), Some(Harness::Grok));
    }

    #[test]
    fn rejects_unknown_harness_text() {
        let dir = tempdir("rejects-unknown");
        fs::create_dir_all(dir.join("config")).unwrap();
        fs::write(config_path(&dir), "cosmo\n").unwrap();
        assert!(load_default_harness(&dir).is_none());
    }

    fn tempdir(label: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("fm-tui-test-{}-{}", std::process::id(), label));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        dir
    }
}
