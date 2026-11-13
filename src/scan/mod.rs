pub mod env;
pub mod idea;
pub mod process;
pub mod project;

use crate::config::Config;
use crate::model::{Inventory, Jdk, Maven, Origin};
use anyhow::Result;
use std::fs;
use std::path::{Path, PathBuf};

pub fn collect(cfg: &Config) -> Result<Inventory> {
    let mut inv = Inventory::default();

    // IDEA first: jdk.table.xml carries friendlier names than raw directory names.
    let idea = idea::discover(cfg);
    for j in idea.jdks {
        push_unique_jdk(&mut inv.jdks, j);
    }
    for m in idea.mavens {
        push_unique_maven(&mut inv.mavens, m);
    }
    env::scan(&mut inv.jdks, &mut inv.mavens);
    let idea_roots = if cfg.strict_roots {
        &[][..]
    } else {
        &idea.project_roots[..]
    };
    inv.projects = project::scan(cfg, idea_roots);
    inv.running = process::scan();
    Ok(inv)
}

/// Expand `$USER_HOME$`, `$APPLICATION_HOME_DIR$` and `~` used by IDEA XML.
pub fn expand_idea_macros(raw: &str, idea_home: Option<&Path>) -> PathBuf {
    let home = dirs::home_dir().unwrap_or_default();
    let mut s = raw.replace("$USER_HOME$", &home.to_string_lossy());
    if let Some(ih) = idea_home {
        s = s.replace("$APPLICATION_HOME_DIR$", &ih.to_string_lossy());
    }
    if s.starts_with('~') {
        s = format!("{}{}", home.to_string_lossy(), &s[1..]);
    }
    PathBuf::from(s.replace('\\', "/"))
}

/// Windows program directories with fallbacks. Some launcher environments (Git Bash,
/// IDE sandboxes) do not inject ProgramFiles/LOCALAPPDATA, so derive them from HOME too.
pub fn windows_program_dirs() -> Vec<PathBuf> {
    let mut out: Vec<PathBuf> = Vec::new();
    for key in ["ProgramFiles", "ProgramFiles(x86)", "LOCALAPPDATA"] {
        if let Ok(v) = std::env::var(key)
            && !v.is_empty()
        {
            out.push(PathBuf::from(v));
        }
    }
    if cfg!(windows) {
        let home = dirs::home_dir().unwrap_or_default();
        out.push(home.join("AppData").join("Local"));
        for drive in ["C", "D", "E"] {
            out.push(PathBuf::from(format!("{drive}:\\Program Files")));
            out.push(PathBuf::from(format!("{drive}:\\Program Files (x86)")));
        }
    }
    out.retain(|p| p.is_dir());
    out.sort();
    out.dedup();
    out
}

/// IDEA XML attributes contain escaped entities such as `&quot;`.
pub fn unescape_xml(s: &str) -> String {
    s.replace("&quot;", "\"")
        .replace("&apos;", "'")
        .replace("&lt;", "<")
        .replace("&gt;", ">")
        .replace("&amp;", "&")
}

pub fn exe(name: &str) -> String {
    if cfg!(windows) {
        format!("{name}.exe")
    } else {
        name.to_string()
    }
}

/// Build a Jdk descriptor without spawning `java -version` (uses the standard `release` file).
pub fn jdk_from_home(home: &Path, name: &str, origin: Origin) -> Option<Jdk> {
    let java = home.join("bin").join(exe("java"));
    if !java.exists() {
        return None;
    }
    let version = java_version(home);
    Some(Jdk {
        name: if name.is_empty() {
            home.file_name()?.to_string_lossy().into_owned()
        } else {
            name.to_string()
        },
        home: home.to_path_buf(),
        version,
        java,
        origin,
    })
}

pub fn java_version(home: &Path) -> String {
    if let Ok(raw) = fs::read_to_string(home.join("release")) {
        for line in raw.lines() {
            if let Some(v) = line.strip_prefix("JAVA_VERSION=") {
                return v.trim_matches('"').trim().to_string();
            }
        }
    }
    let name = home
        .file_name()
        .map(|s| s.to_string_lossy().into_owned())
        .unwrap_or_default();
    let digits: String = name
        .chars()
        .skip_while(|c| !c.is_ascii_digit())
        .take_while(|c| c.is_ascii_digit() || *c == '.')
        .collect();
    if digits.is_empty() {
        "unknown".into()
    } else {
        digits
    }
}

pub fn maven_from_home(home: &Path, name: &str, origin: Origin) -> Option<Maven> {
    let bin = home.join("bin");
    let candidates = if cfg!(windows) {
        vec![bin.join("mvn.cmd"), bin.join("mvn.bat")]
    } else {
        vec![bin.join("mvn")]
    };
    let executable = candidates.into_iter().find(|p| p.exists())?;
    let version = maven_version(home);
    Some(Maven {
        name: if name.is_empty() {
            format!("maven {}", version)
        } else {
            name.to_string()
        },
        home: home.to_path_buf(),
        version,
        executable,
        origin,
        wrapper: false,
    })
}

/// IDEA bundles `lib/maven-core-<version>.jar`; reading the filename is faster and safer
/// than spawning `mvn -v` during a scan.
pub fn maven_version(home: &Path) -> String {
    if let Ok(rd) = fs::read_dir(home.join("lib")) {
        for entry in rd.flatten() {
            let n = entry.file_name().to_string_lossy().into_owned();
            if let Some(rest) = n.strip_prefix("maven-core-")
                && let Some(v) = rest.strip_suffix(".jar")
            {
                return v.to_string();
            }
        }
    }
    "unknown".into()
}

pub fn push_unique_jdk(list: &mut Vec<Jdk>, item: Jdk) {
    let key = normalize(&item.home);
    if !list.iter().any(|j| normalize(&j.home) == key) {
        list.push(item);
    } else if let Some(slot) = list.iter_mut().find(|j| normalize(&j.home) == key) {
        if slot.version == "unknown" && item.version != "unknown" {
            slot.version = item.version;
        }
        if slot.origin == Origin::System && item.origin != Origin::System {
            slot.origin = item.origin;
            slot.name = item.name;
        }
    }
}

pub fn push_unique_maven(list: &mut Vec<Maven>, item: Maven) {
    let key = normalize(&item.home);
    if !list.iter().any(|m| normalize(&m.home) == key) {
        list.push(item);
    }
}

fn normalize(p: &Path) -> String {
    let canonical = fs::canonicalize(p).unwrap_or_else(|_| p.to_path_buf());
    canonical
        .to_string_lossy()
        .trim_start_matches(r"\\?\")
        .replace('\\', "/")
        .to_lowercase()
}
