use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(default)]
pub struct Config {
    /// Project scan roots. Empty (the default) means: index the repository
    /// containing the current working directory.
    pub scan_roots: Vec<PathBuf>,
    pub max_depth: usize,
    /// Prefix chord. Supported: ctrl+b, ctrl+a, ctrl+space.
    pub prefix: String,
    /// Preferred toolchain by name (matched case-insensitively, substring OK).
    pub preferred_jdk: Option<String>,
    pub preferred_maven: Option<String>,
    /// Extra arguments appended to every launch (e.g. --spring.profiles.active=dev).
    pub extra_args: Vec<String>,
    /// Lines kept in the in-memory log ring buffer per process.
    pub log_capacity: usize,
    /// Milliseconds between automatic inventory refresh / log redraw.
    pub tick_ms: u64,
    /// Skip IDE-bundled toolchains that are too old for the project.
    pub skip_jbr: bool,
    /// Ignore IDEA's historical project roots; index only `scan_roots` / cwd.
    pub strict_roots: bool,
    /// List only modules that can actually be launched (own Spring Boot main
    /// class). Falls back to all Spring modules when a tree has none.
    pub launchable_only: bool,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            scan_roots: Vec::new(),
            max_depth: 6,
            prefix: "ctrl+b".into(),
            preferred_jdk: None,
            preferred_maven: None,
            extra_args: vec![],
            log_capacity: 2000,
            tick_ms: 250,
            skip_jbr: false,
            strict_roots: true,
            launchable_only: true,
        }
    }
}

impl Config {
    pub fn path() -> PathBuf {
        dirs::config_dir()
            .unwrap_or_else(|| PathBuf::from("."))
            .join("tspb")
            .join("config.toml")
    }

    pub fn load() -> Result<Self> {
        let path = Self::path();
        if !path.exists() {
            return Ok(Self::default());
        }
        let raw = std::fs::read_to_string(&path)
            .with_context(|| format!("reading config {}", path.display()))?;
        toml::from_str(&raw).with_context(|| format!("parsing config {}", path.display()))
    }

    pub fn save_default(path: &Path) -> Result<()> {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        std::fs::write(path, toml::to_string_pretty(&Self::default())?)?;
        Ok(())
    }

    /// Classic global discovery roots, used by `--wide`.
    pub fn global_roots() -> Vec<PathBuf> {
        let home = dirs::home_dir().unwrap_or_else(|| PathBuf::from("."));
        let mut roots = vec![std::env::current_dir().unwrap_or_else(|_| home.clone())];
        // Generic code directories only; no machine-specific layout is baked in.
        for candidate in [
            home.join("IdeaProjects"),
            home.join("workspace"),
            home.join("Projects"),
            home.join("dev"),
            home.join("code"),
            home.join("src"),
        ] {
            if candidate.is_dir() && !roots.contains(&candidate) {
                roots.push(candidate);
            }
        }
        roots
    }

    /// Roots actually used for project scanning.
    pub fn effective_roots(&self) -> Vec<PathBuf> {
        if self.scan_roots.is_empty() {
            let cwd = std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."));
            vec![crate::scan::project::locate_repo_root(&cwd)]
        } else {
            self.scan_roots.clone()
        }
    }

    /// The primary root, shown in the header / scan output.
    pub fn effective_root(&self) -> PathBuf {
        self.effective_roots()
            .into_iter()
            .next()
            .unwrap_or_else(|| PathBuf::from("."))
    }
}
