use crate::config::Config;
use crate::model::{BuildTool, LaunchConfig, Origin, Project};
use crate::progress::Reporter;
use quick_xml::Reader;
use quick_xml::events::Event;
use std::collections::HashSet;
use std::fs;
use std::path::{Path, PathBuf};
use walkdir::{DirEntry, WalkDir};

const SKIP_DIRS: &[&str] = &[
    ".git",
    ".idea",
    ".gradle",
    "target",
    "build",
    "out",
    "node_modules",
    "dist",
    ".venv",
    "venv",
    "__pycache__",
    ".mvn",
    "bin",
    "obj",
    "vendor",
    ".next",
    "logs",
    "coverage",
    ".cache",
    "cmake-build-debug",
];

/// Walk up from `cwd` to the enclosing repository root. A `.git` directory is a
/// hard boundary; otherwise the outermost Maven/Gradle aggregator or `.idea`
/// checkout wins. Never crosses the user's home or a drive root.
pub fn locate_repo_root(cwd: &Path) -> PathBuf {
    let home = dirs::home_dir().unwrap_or_default();
    let mut best: Option<PathBuf> = None;
    let mut cur = Some(cwd);
    for _ in 0..=5 {
        let Some(dir) = cur else { break };
        if dir == home.as_path() || dir.parent().is_none() {
            break;
        }
        if dir.join(".git").exists() {
            return dir.to_path_buf();
        }
        if is_aggregator_pom(&dir.join("pom.xml"))
            || dir.join("settings.gradle").is_file()
            || dir.join("settings.gradle.kts").is_file()
            || dir.join(".idea").is_dir()
        {
            best = Some(dir.to_path_buf());
        }
        cur = dir.parent();
    }
    best.unwrap_or_else(|| cwd.to_path_buf())
}

fn is_aggregator_pom(path: &Path) -> bool {
    fs::read_to_string(path).is_ok_and(|s| s.contains("<modules>") || s.contains("<module>"))
}

pub fn scan(cfg: &Config, idea_roots: &[PathBuf], rep: &Reporter) -> Vec<Project> {
    let mut roots: Vec<PathBuf> = cfg.effective_roots();
    if !cfg.strict_roots {
        roots.extend(idea_roots.iter().cloned());
    }
    let mut seen = HashSet::new();
    // (Project, launchable) keeps the loose fallback available for unusual trees.
    let mut found: Vec<(Project, bool)> = Vec::new();
    // Directories visited across all roots; drives the startup progress gauge.
    let mut visited = 0usize;

    for root in roots {
        if !root.is_dir() || found.len() >= 400 {
            continue;
        }
        rep.detail(root.display().to_string());
        // In a multi-module Maven project all IDEA run configurations live in the
        // repository root's .idea/, not in each module directory.
        let root_configs = root_run_configs(&root);
        let walker = WalkDir::new(&root)
            .max_depth(cfg.max_depth)
            .follow_links(false)
            .into_iter()
            .filter_entry(|e| !is_skipped(e));
        for entry in walker.filter_map(Result::ok) {
            visited += 1;
            if visited.is_multiple_of(64) {
                // Cheap progress signal: one lock bump per 64 entries.
                rep.dirs(visited);
            }
            if !entry.file_type().is_file() || found.len() >= 400 {
                continue;
            }
            let fname = entry.file_name().to_string_lossy();
            let build = match fname.as_ref() {
                "pom.xml" => BuildTool::Maven,
                "build.gradle" | "build.gradle.kts" => BuildTool::Gradle,
                _ => continue,
            };
            let dir = match entry.path().parent() {
                Some(d) => d.to_path_buf(),
                None => continue,
            };
            let key = fs::canonicalize(&dir)
                .unwrap_or_else(|_| dir.clone())
                .to_string_lossy()
                .to_lowercase();
            if !seen.insert(key) {
                continue;
            }
            let raw = fs::read_to_string(entry.path()).unwrap_or_default();
            if !looks_like_spring_boot(&raw, &dir) {
                continue;
            }
            let name = dir
                .file_name()
                .map(|s| s.to_string_lossy().into_owned())
                .unwrap_or_else(|| "project".into());
            let has_wrapper = ["mvnw", "mvnw.cmd", "gradlew", "gradlew.bat"]
                .iter()
                .any(|w| dir.join(w).exists());
            let main_class = find_main_class(&dir);
            // A launch item needs its own Spring Boot main class. Parent poms,
            // api/biz libraries and aggregators merely depend on Spring Boot.
            let launchable =
                main_class.is_some() && xml_tag_value(&raw, "packaging").as_deref() != Some("pom");
            let java_version = java_version_of(&dir);
            let mut launch_configs = read_run_configurations(&dir);
            for c in &root_configs {
                if let Some(m) = c.main_class.as_deref()
                    && main_class.as_deref() == Some(m)
                    && !launch_configs.iter().any(|x| x.name == c.name)
                {
                    launch_configs.push(c.clone());
                }
            }
            launch_configs.push(default_launch_config(build, main_class.as_deref()));
            launch_configs.dedup_by(|a, b| a.name == b.name);
            let origin = if idea_roots.iter().any(|r| dir.starts_with(r)) {
                Origin::Idea
            } else {
                Origin::Project
            };

            found.push((
                Project {
                    name,
                    path: dir,
                    build,
                    has_wrapper,
                    main_class,
                    java_version,
                    launch_configs,
                    origin,
                },
                launchable,
            ));
            rep.projects(found.len());
        }
    }
    rep.dirs(visited);

    let mut out: Vec<Project> = if cfg.launchable_only {
        let strict: Vec<Project> = found
            .iter()
            .filter(|(_, launchable)| *launchable)
            .map(|(p, _)| p.clone())
            .collect();
        if strict.is_empty() {
            // Kotlin / generated-source / main-class-in-a-dependency layouts
            // still deserve a populated picker instead of an empty TUI.
            found.into_iter().map(|(p, _)| p).collect()
        } else {
            strict
        }
    } else {
        found.into_iter().map(|(p, _)| p).collect()
    };
    out.sort_by_key(|a| a.name.to_lowercase());
    out
}

fn is_skipped(e: &DirEntry) -> bool {
    if e.depth() == 0 {
        return false;
    }
    e.file_type().is_dir()
        && e.file_name()
            .to_str()
            .is_some_and(|n| SKIP_DIRS.contains(&n) || n.starts_with('.'))
}

fn looks_like_spring_boot(raw: &str, dir: &Path) -> bool {
    if raw.contains("spring-boot")
        || raw.contains("org.springframework.boot")
        || raw.contains("springboot")
    {
        return true;
    }
    let src = dir.join("src").join("main").join("java");
    if !src.is_dir() {
        return false;
    }
    WalkDir::new(src)
        .max_depth(5)
        .into_iter()
        .filter_map(Result::ok)
        .take(60)
        .any(|e| {
            e.file_name()
                .to_string_lossy()
                .ends_with("Application.java")
                && fs::read_to_string(e.path())
                    .map(|s| s.contains("@SpringBootApplication"))
                    .unwrap_or(false)
        })
}

/// Read the declared Java version from this pom or its parent poms.
/// Multi-module Maven projects usually declare `<java.version>` only in the root pom.
pub fn java_version_of(dir: &Path) -> Option<u32> {
    let mut cur = Some(dir);
    for _ in 0..=4 {
        let Some(d) = cur else { break };
        if let Ok(raw) = fs::read_to_string(d.join("pom.xml")) {
            for tag in [
                "java.version",
                "maven.compiler.release",
                "maven.compiler.source",
            ] {
                if let Some(v) = xml_tag_value(&raw, tag)
                    && let Some(major) = parse_major(&v)
                {
                    return Some(major);
                }
            }
        }
        cur = d.parent();
    }
    None
}

fn xml_tag_value(raw: &str, tag: &str) -> Option<String> {
    let open = format!("<{tag}>");
    let close = format!("</{tag}>");
    let start = raw.find(&open)? + open.len();
    let end = raw[start..].find(&close)? + start;
    let value = raw[start..end].trim();
    // Property references such as ${java.version} need the parent pom instead.
    (!value.is_empty() && !value.starts_with("${")).then(|| value.to_string())
}

fn parse_major(v: &str) -> Option<u32> {
    let digits: String = v
        .chars()
        .take_while(|c| c.is_ascii_digit() || *c == '.')
        .collect();
    let mut parts = digits.split('.');
    let mut major: u32 = parts.next()?.parse().ok()?;
    if major == 1 {
        major = parts.next()?.parse().ok()?; // 1.8 -> 8
    }
    Some(major)
}

/// Resolve `package x.y; ... @SpringBootApplication public class DemoApplication` -> `x.y.DemoApplication`.
fn find_main_class(dir: &Path) -> Option<String> {
    let mut roots = vec![dir.join("src").join("main").join("java")];
    roots.push(dir.join("src").join("main").join("kotlin"));
    for root in roots.iter().filter(|r| r.is_dir()) {
        for entry in WalkDir::new(root)
            .max_depth(8)
            .into_iter()
            .filter_map(Result::ok)
            .take(400)
        {
            if !entry.file_type().is_file() {
                continue;
            }
            let name = entry.file_name().to_string_lossy();
            if !name.ends_with(".java") && !name.ends_with(".kt") {
                continue;
            }
            let Ok(raw) = fs::read_to_string(entry.path()) else {
                continue;
            };
            if !raw.contains("@SpringBootApplication") {
                continue;
            }
            let package = raw
                .lines()
                .find_map(|l| {
                    l.trim()
                        .strip_prefix("package ")
                        .map(|s| s.trim().trim_end_matches(';'))
                })
                .unwrap_or("")
                .to_string();
            let class = name.trim_end_matches(".java").trim_end_matches(".kt");
            return Some(if package.is_empty() {
                class.to_string()
            } else {
                format!("{package}.{class}")
            });
        }
    }
    None
}

fn default_launch_config(build: BuildTool, main: Option<&str>) -> LaunchConfig {
    LaunchConfig {
        name: match build {
            BuildTool::Maven => "spring-boot:run".into(),
            BuildTool::Gradle => "bootRun".into(),
        },
        main_class: main.map(str::to_string),
        module_dir: None,
        vm_options: String::new(),
        program_args: String::new(),
        active_profiles: String::new(),
        origin: "auto".into(),
    }
}

/// Collect IDEA run configurations from the nearest enclosing `.idea` directory.
/// Multi-module repositories keep the RunManager in the checkout root, several
/// levels above the individual `-start` modules.
fn root_run_configs(root: &Path) -> Vec<LaunchConfig> {
    let mut cur = Some(root);
    for _ in 0..=4 {
        let Some(dir) = cur else { break };
        let idea = dir.join(".idea");
        if idea.is_dir() {
            let mut files = Vec::new();
            if let Ok(rd) = fs::read_dir(idea.join("runConfigurations")) {
                for e in rd.flatten() {
                    if e.path().extension().is_some_and(|x| x == "xml") {
                        files.push(e.path());
                    }
                }
            }
            let workspace = idea.join("workspace.xml");
            if workspace.is_file() {
                files.push(workspace);
            }
            let mut out = Vec::new();
            for f in files {
                parse_run_xml(&f, &mut out);
            }
            return out;
        }
        cur = dir.parent();
    }
    Vec::new()
}

/// Read `.idea/runConfigurations/*.xml` and `.idea/workspace.xml` RunManager entries.
fn read_run_configurations(dir: &Path) -> Vec<LaunchConfig> {
    let mut files: Vec<PathBuf> = Vec::new();
    let rc_dir = dir.join(".idea").join("runConfigurations");
    if let Ok(rd) = fs::read_dir(rc_dir) {
        for e in rd.flatten() {
            if e.path().extension().is_some_and(|x| x == "xml") {
                files.push(e.path());
            }
        }
    }
    let workspace = dir.join(".idea").join("workspace.xml");
    if workspace.is_file() {
        files.push(workspace);
    }
    let mut out = Vec::new();
    for f in files {
        parse_run_xml(&f, &mut out);
    }
    out
}

fn parse_run_xml(path: &Path, out: &mut Vec<LaunchConfig>) {
    let Ok(raw) = fs::read_to_string(path) else {
        return;
    };
    let mut reader = Reader::from_str(&raw);
    reader.config_mut().trim_text(true);
    let mut in_cfg = false;
    let mut current: Option<LaunchConfig> = None;

    let commit = |cfg: Option<LaunchConfig>, list: &mut Vec<LaunchConfig>| {
        if let Some(c) = cfg
            && !list.iter().any(|x: &LaunchConfig| x.name == c.name)
        {
            list.push(c);
        }
    };

    while let Ok(event) = reader.read_event() {
        // Both Start and Empty carry the same BytesStart payload; track self-closing
        // elements so a `<configuration ... />` is committed without waiting for End.
        let (e, is_empty) = match event {
            Event::Start(x) => (x, false),
            Event::Empty(x) => (x, true),
            Event::End(x) => {
                if x.name().as_ref() == "configuration" {
                    commit(current.take(), out);
                    in_cfg = false;
                }
                continue;
            }
            Event::Eof => break,
            _ => continue,
        };

        let tag = e.name().as_ref().to_string();
        if tag == "configuration" {
            let is_default = e
                .attributes()
                .flatten()
                .any(|a| a.key.as_ref() == "default" && a.value.as_ref() == "true");
            if is_default {
                continue;
            }
            let ctype = attr(&e, "type").unwrap_or_default();
            // IDEA records Spring Boot mains as plain `Application` configurations
            // (often temporary=true). They are accepted here and later bound to a
            // module only when MAIN_CLASS_NAME matches that module's main class.
            let is_spring = ctype == "SpringBootApplicationConfigurationType"
                || ctype == "SpringApplicationConfigurationType"
                || ctype == "Application"
                || (ctype == "MavenRunConfiguration" && raw.contains("spring-boot:run"))
                || (ctype == "GradleRunConfiguration" && raw.contains("bootRun"));
            if is_spring {
                in_cfg = true;
                let c = LaunchConfig {
                    name: attr(&e, "name").unwrap_or_else(|| "Spring Boot".into()),
                    main_class: attr(&e, "MAIN_CLASS_NAME"),
                    module_dir: None,
                    vm_options: String::new(),
                    program_args: String::new(),
                    active_profiles: String::new(),
                    origin: "IDEA run configuration".into(),
                };
                if is_empty {
                    commit(Some(c), out);
                    in_cfg = false;
                } else {
                    current = Some(c);
                }
            }
        } else if in_cfg && tag == "option" {
            let key = attr(&e, "name").unwrap_or_default();
            let val = attr(&e, "value").unwrap_or_default();
            if let Some(c) = current.as_mut() {
                match key.as_str() {
                    "MAIN_CLASS_NAME" | "SPRING_BOOT_MAIN_CLASS" if !val.is_empty() => {
                        c.main_class = Some(val);
                    }
                    "VM_PARAMETERS" => c.vm_options = val,
                    "PROGRAM_PARAMETERS" => c.program_args = val,
                    "ACTIVE_PROFILES" => c.active_profiles = val,
                    _ => {}
                }
            }
        }
    }
    commit(current.take(), out);
}

fn attr(e: &quick_xml::events::BytesStart<'_>, key: &str) -> Option<String> {
    e.attributes()
        .flatten()
        .find(|a| a.key.as_ref() == key)
        .map(|a| crate::scan::unescape_xml(a.value.as_ref()))
}
