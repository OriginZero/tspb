use super::{exe, jdk_from_home, maven_from_home, push_unique_jdk, push_unique_maven};
use crate::model::{Jdk, Maven, Origin};
use std::fs;
use std::path::{Path, PathBuf};

/// Scan JDK / Maven from environment variables, PATH, and well-known install roots.
/// Never spawns an external process: versions come from `release` and `maven-core-*.jar`.
pub fn scan(jdks: &mut Vec<Jdk>, mavens: &mut Vec<Maven>) {
    scan_env_vars(jdks, mavens);
    scan_path(jdks, mavens);
    scan_install_roots(jdks, mavens);
}

fn scan_env_vars(jdks: &mut Vec<Jdk>, mavens: &mut Vec<Maven>) {
    for key in ["JAVA_HOME", "JDK_HOME", "JAVA_HOME_17", "JAVA_HOME_21"] {
        if let Ok(v) = std::env::var(key)
            && !v.trim().is_empty()
        {
            let home = PathBuf::from(v.trim().trim_matches('"'));
            if let Some(j) = jdk_from_home(&home, key, Origin::Env) {
                push_unique_jdk(jdks, j);
            }
        }
    }
    for key in ["MAVEN_HOME", "M2_HOME", "M3_HOME", "MAVEN_ROOT"] {
        if let Ok(v) = std::env::var(key)
            && !v.trim().is_empty()
        {
            let home = PathBuf::from(v.trim().trim_matches('"'));
            if let Some(m) = maven_from_home(&home, key, Origin::Env) {
                push_unique_maven(mavens, m);
            }
        }
    }
}

fn scan_path(jdks: &mut Vec<Jdk>, mavens: &mut Vec<Maven>) {
    for name in ["java", "javac"] {
        if let Ok(all) = which::which_all(name) {
            for p in all {
                if let Some(home) = parent_home(&p) {
                    let label = format!("PATH:{name}");
                    if let Some(j) = jdk_from_home(&home, &label, Origin::Env) {
                        push_unique_jdk(jdks, j);
                    }
                }
            }
        }
    }
    for name in ["mvn", "mvnw", "gradle"] {
        if let Ok(all) = which::which_all(name) {
            for p in all {
                if let Some(home) = parent_home(&p)
                    && let Some(m) = maven_from_home(&home, &format!("PATH:{name}"), Origin::Env)
                {
                    let mut m = m;
                    m.wrapper = name == "mvnw";
                    push_unique_maven(mavens, m);
                }
            }
        }
    }
}

/// `/some/jdk/bin/java` -> `/some/jdk`; canonicalizes symlinks (e.g. /usr/bin/mvn).
fn parent_home(exe_path: &Path) -> Option<PathBuf> {
    let resolved = fs::canonicalize(exe_path).unwrap_or_else(|_| exe_path.to_path_buf());
    let bin = resolved.parent()?;
    if bin
        .file_name()?
        .to_string_lossy()
        .eq_ignore_ascii_case("bin")
    {
        bin.parent().map(|p| p.to_path_buf())
    } else {
        None
    }
}

/// Enumerate children of well-known JDK/Maven install roots (one level deep).
fn scan_install_roots(jdks: &mut Vec<Jdk>, mavens: &mut Vec<Maven>) {
    let home = dirs::home_dir().unwrap_or_default();
    let mut jdk_roots: Vec<PathBuf> = Vec::new();
    let mut mvn_roots: Vec<PathBuf> = Vec::new();

    if cfg!(windows) {
        for base in super::windows_program_dirs() {
            for sub in [
                "Java",
                "Eclipse Adoptium",
                "Microsoft",
                "Zulu",
                "Amazon Corretto",
            ] {
                jdk_roots.push(base.join(sub));
            }
            mvn_roots.push(base.join("Apache").join("maven"));
        }
        jdk_roots.push(home.join(".jdks"));
        mvn_roots.push(home.join("scoop").join("apps").join("maven"));
    } else {
        jdk_roots.push(PathBuf::from("/usr/lib/jvm"));
        jdk_roots.push(PathBuf::from("/opt/java"));
        jdk_roots.push(PathBuf::from("/Library/Java/JavaVirtualMachines"));
        mvn_roots.push(PathBuf::from("/usr/share/maven"));
        mvn_roots.push(PathBuf::from("/opt/maven"));
        mvn_roots.push(PathBuf::from("/opt/homebrew/Cellar/maven"));
    }
    jdk_roots.push(home.join(".sdkman").join("candidates").join("java"));
    mvn_roots.push(home.join(".sdkman").join("candidates").join("maven"));

    for root in jdk_roots.iter().filter(|r| r.is_dir()) {
        let origin = if root.to_string_lossy().contains("sdkman") {
            Origin::Sdkman
        } else if root.ends_with(".jdks") {
            // IDEA downloads project JDKs here.
            Origin::Idea
        } else {
            Origin::System
        };
        // The root itself may be a JDK (e.g. JAVA_HOME layout).
        if let Some(j) = jdk_from_home(root, "", origin) {
            push_unique_jdk(jdks, j);
        }
        if let Ok(rd) = fs::read_dir(root) {
            for entry in rd.flatten() {
                let p = entry.path();
                if !p.is_dir() {
                    continue;
                }
                // macOS bundles live in Contents/Home.
                let home_dir = p.join("Contents").join("Home");
                let candidate = if home_dir.is_dir() { &home_dir } else { &p };
                if let Some(j) = jdk_from_home(candidate, "", origin) {
                    push_unique_jdk(jdks, j);
                }
            }
        }
    }

    for root in mvn_roots.iter().filter(|r| r.is_dir()) {
        let origin = if root.to_string_lossy().contains("sdkman") {
            Origin::Sdkman
        } else {
            Origin::System
        };
        if let Some(m) = maven_from_home(root, "", origin) {
            push_unique_maven(mavens, m);
        }
        if let Ok(rd) = fs::read_dir(root) {
            for entry in rd.flatten() {
                let p = entry.path();
                if p.is_dir()
                    && let Some(m) = maven_from_home(&p, "", origin)
                {
                    push_unique_maven(mavens, m);
                }
            }
        }
    }
}

/// `java` may live in a `jre/bin` subfolder; used when resolving a java executable
/// found in a running process command line.
pub fn home_from_java_exe(path: &Path) -> Option<PathBuf> {
    let resolved = fs::canonicalize(path).unwrap_or_else(|_| path.to_path_buf());
    let bin = resolved.parent()?;
    let home = bin.parent()?;
    if home.join("bin").join(exe("java")).exists() || home.join("release").exists() {
        Some(home.to_path_buf())
    } else {
        None
    }
}
