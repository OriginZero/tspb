use super::{
    expand_idea_macros, jdk_from_home, maven_from_home, unescape_xml, windows_program_dirs,
};
use crate::config::Config;
use crate::model::{Jdk, Maven, Origin};
use quick_xml::Reader;
use quick_xml::events::Event;
use std::fs;
use std::path::{Path, PathBuf};

#[derive(Default)]
pub struct IdeaInfo {
    pub homes: Vec<PathBuf>,
    pub config_dirs: Vec<PathBuf>,
    /// Absolute project paths harvested from recentProjects.xml / workspaces.xml.
    pub project_roots: Vec<PathBuf>,
    pub jdks: Vec<Jdk>,
    pub mavens: Vec<Maven>,
}

pub fn discover(cfg: &Config) -> IdeaInfo {
    let mut info = IdeaInfo {
        homes: find_installations(),
        config_dirs: find_config_dirs(),
        ..Default::default()
    };

    for home in &info.homes {
        // JetBrains Runtime: a full JDK shipped with the IDE.
        if !cfg.skip_jbr {
            let jbr = home.join("jbr");
            if let Some(mut j) = jdk_from_home(&jbr, "", Origin::Idea) {
                j.name = format!("IDEA JBR {}", major(&j.version));
                info.jdks.push(j);
            }
        }
        // Bundled Maven moved from plugins/maven to plugins/maven-plugin in recent IDEs.
        for rel in [
            "plugins/maven-plugin/lib/maven3",
            "plugins/maven/lib/maven3",
            "plugins/maven/lib/maven",
        ] {
            let mvn_home = home.join(rel);
            if let Some(mut m) = maven_from_home(&mvn_home, "", Origin::Idea) {
                m.name = format!("IDEA Maven {}", m.version);
                info.mavens.push(m);
            }
        }
    }

    for dir in &info.config_dirs {
        read_jdk_table(&dir.join("options").join("jdk.table.xml"), &mut info.jdks);
        read_recent_projects(
            &dir.join("options").join("recentProjects.xml"),
            &mut info.project_roots,
        );
        read_recent_projects(
            &dir.join("options").join("workspaces.xml"),
            &mut info.project_roots,
        );
    }

    // IDEA-managed JDK downloads under ~/.jdks.
    let jdks_dir = dirs::home_dir().unwrap_or_default().join(".jdks");
    if jdks_dir.is_dir()
        && let Ok(rd) = fs::read_dir(&jdks_dir)
    {
        for entry in rd.flatten() {
            let p = entry.path();
            if p.is_dir()
                && let Some(j) = jdk_from_home(&p, "", Origin::Idea)
            {
                info.jdks.push(j);
            }
        }
    }

    info.jdks.sort_by(|a, b| a.name.cmp(&b.name));
    info.project_roots.dedup();
    info
}

fn major(v: &str) -> String {
    v.split('.').next().unwrap_or(v).to_string()
}

fn push_unique_path(list: &mut Vec<PathBuf>, p: PathBuf) {
    let key = p.to_string_lossy().replace('\\', "/").to_lowercase();
    if p.exists()
        && !list
            .iter()
            .any(|x| x.to_string_lossy().replace('\\', "/").to_lowercase() == key)
    {
        list.push(p);
    }
}

fn find_installations() -> Vec<PathBuf> {
    let mut out = Vec::new();
    let home = dirs::home_dir().unwrap_or_default();
    let mut bases: Vec<PathBuf> = Vec::new();
    if cfg!(windows) {
        for base in windows_program_dirs() {
            bases.push(base.join("JetBrains"));
        }
        bases.push(
            home.join("AppData")
                .join("Local")
                .join("JetBrains")
                .join("Toolbox")
                .join("apps"),
        );
    } else {
        bases.push(PathBuf::from("/opt"));
        bases.push(home.join("Applications"));
        bases.push(
            home.join(".local")
                .join("share")
                .join("JetBrains")
                .join("Toolbox")
                .join("apps"),
        );
    }
    for base in bases.iter().filter(|b| b.is_dir()) {
        if let Ok(rd) = fs::read_dir(base) {
            for entry in rd.flatten() {
                let p = entry.path();
                if !p.is_dir() {
                    continue;
                }
                let n = p
                    .file_name()
                    .unwrap_or_default()
                    .to_string_lossy()
                    .to_lowercase();
                if n.contains("idea") || n.contains("intellij") {
                    if p.join("jbr").is_dir() || p.join("plugins").is_dir() {
                        push_unique_path(&mut out, p.clone());
                    }
                    // Toolbox nests the actual install in ch-0/<build>/.
                    for sub in ["ch-0", "ch-1"] {
                        let ch = p.join(sub);
                        if let Ok(inner) = fs::read_dir(&ch) {
                            for e in inner.flatten() {
                                let real = e.path();
                                if real.join("jbr").is_dir() {
                                    push_unique_path(&mut out, real);
                                }
                            }
                        }
                    }
                }
            }
        }
    }
    out
}

fn find_config_dirs() -> Vec<PathBuf> {
    let mut out = Vec::new();
    let mut roots: Vec<PathBuf> = Vec::new();
    let home = dirs::home_dir().unwrap_or_default();
    if cfg!(windows) {
        if let Ok(v) = std::env::var("APPDATA") {
            roots.push(PathBuf::from(v).join("JetBrains"));
        }
        roots.push(home.join("AppData").join("Roaming").join("JetBrains"));
        if let Ok(v) = std::env::var("LOCALAPPDATA") {
            roots.push(PathBuf::from(v).join("JetBrains"));
        }
        roots.push(home.join("AppData").join("Local").join("JetBrains"));
    } else if cfg!(target_os = "macos") {
        roots.push(home.join("Library/Application Support/JetBrains"));
    } else {
        roots.push(dirs::config_dir().unwrap_or_default().join("JetBrains"));
    }
    for root in roots.iter().filter(|r| r.is_dir()) {
        if let Ok(rd) = fs::read_dir(root) {
            for entry in rd.flatten() {
                let p = entry.path();
                let n = p
                    .file_name()
                    .unwrap_or_default()
                    .to_string_lossy()
                    .to_lowercase();
                if p.is_dir() && (n.starts_with("idea") || n.starts_with("intellij")) {
                    push_unique_path(&mut out, p);
                }
            }
        }
    }
    out
}

fn attr(e: &quick_xml::events::BytesStart<'_>, key: &str) -> Option<String> {
    e.attributes()
        .flatten()
        .find(|a| a.key.as_ref() == key)
        .map(|a| unescape_xml(a.value.as_ref()))
}

/// Parse `jdk.table.xml`: <jdk><name value=".."/><type value="JavaSDK"/><homePath value=".."/>...
fn read_jdk_table(path: &Path, jdks: &mut Vec<Jdk>) {
    let Ok(raw) = fs::read_to_string(path) else {
        return;
    };
    let mut reader = Reader::from_str(&raw);
    reader.config_mut().trim_text(true);
    let mut in_jdk = false;
    let mut name = String::new();
    let mut version = String::new();
    let mut home_path = String::new();
    let mut sdk_type = String::new();

    loop {
        match reader.read_event() {
            Ok(Event::Start(e)) if e.name().as_ref() == "jdk" => {
                in_jdk = true;
                name.clear();
                version.clear();
                home_path.clear();
                sdk_type.clear();
            }
            Ok(Event::Start(e)) | Ok(Event::Empty(e)) if in_jdk => {
                if let Some(v) = attr(&e, "value") {
                    match e.name().as_ref() {
                        "name" => name = v,
                        "type" => sdk_type = v,
                        "version" => version = v,
                        "homePath" => home_path = v,
                        _ => {}
                    }
                }
            }
            Ok(Event::End(e)) if e.name().as_ref() == "jdk" => {
                in_jdk = false;
                if sdk_type != "JavaSDK" || home_path.is_empty() {
                    continue;
                }
                let home = expand_idea_macros(&home_path, None);
                if let Some(mut j) = jdk_from_home(&home, &name, Origin::Idea) {
                    if !version.is_empty() {
                        j.name = format!("{} ({})", name, short_version(&version));
                    }
                    let key = j.home.to_string_lossy().replace('\\', "/").to_lowercase();
                    if !jdks
                        .iter()
                        .any(|x| x.home.to_string_lossy().replace('\\', "/").to_lowercase() == key)
                    {
                        jdks.push(j);
                    }
                }
            }
            Ok(Event::Eof) | Err(_) => break,
            _ => {}
        }
    }
}

fn short_version(v: &str) -> String {
    // 'java version "1.8.0_482"' -> 1.8.0_482 ; 'BellSoft Liberica 17.0.21' -> 17.0.21
    let cleaned = v.replace('"', " ");
    for token in cleaned.split_whitespace() {
        if token.chars().next().is_some_and(|c| c.is_ascii_digit()) {
            return token
                .trim_matches(|c: char| !(c.is_ascii_alphanumeric() || c == '.' || c == '_'))
                .to_string();
        }
    }
    v.to_string()
}

/// Parse `<entry key="$USER_HOME$/...">` and `<option value="...">` project paths.
fn read_recent_projects(path: &Path, roots: &mut Vec<PathBuf>) {
    let Ok(raw) = fs::read_to_string(path) else {
        return;
    };
    let mut reader = Reader::from_str(&raw);
    reader.config_mut().trim_text(true);
    loop {
        match reader.read_event() {
            Ok(Event::Start(e)) | Ok(Event::Empty(e)) => {
                let tag = e.name();
                let raw_path = if tag.as_ref() == "entry" {
                    attr(&e, "key")
                } else if tag.as_ref() == "option" {
                    attr(&e, "value")
                } else {
                    None
                };
                if let Some(v) = raw_path
                    && (v.contains('$') || v.starts_with('/') || v.contains(":\\"))
                {
                    let p = expand_idea_macros(&v, None);
                    if p.is_dir() || v.contains("$USER_HOME$") {
                        push_unique_path(roots, p);
                    }
                }
            }
            Ok(Event::Eof) | Err(_) => break,
            _ => {}
        }
    }
}
