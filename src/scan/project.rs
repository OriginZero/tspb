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
    fs::read_to_string(path).is_ok_and(|s| {
        let clean = strip_xml_comments(&s);
        clean.contains("<modules>") || clean.contains("<module>")
    })
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
            let clean_pom = (build == BuildTool::Maven).then(|| strip_xml_comments(&raw));
            let is_pom_packaging = clean_pom
                .as_deref()
                .and_then(|s| xml_tag_value(s, "packaging"))
                .as_deref()
                == Some("pom");
            let artifact_id = clean_pom.as_deref().and_then(pom_artifact_id);
            // A launch item needs its own Spring Boot main class. Parent poms,
            // api/biz libraries and aggregators merely depend on Spring Boot.
            let launchable = main_class.is_some() && !is_pom_packaging;
            let java_version = java_version_of(&dir);
            let mut launch_configs: Vec<LaunchConfig> = read_run_configurations(&dir)
                .into_iter()
                .filter(|c| {
                    config_matches_local_dir(
                        c,
                        &name,
                        artifact_id.as_deref(),
                        main_class.as_deref(),
                        is_pom_packaging,
                    )
                })
                .collect();
            for c in &root_configs {
                if config_matches_module(
                    c,
                    &name,
                    artifact_id.as_deref(),
                    main_class.as_deref(),
                    is_pom_packaging,
                ) && !launch_configs.iter().any(|x| x.name == c.name)
                {
                    launch_configs.push(c.clone());
                }
            }
            let default_cfg = default_launch_config(build, main_class.as_deref());
            if !launch_configs.iter().any(|x| x.name == default_cfg.name) {
                launch_configs.push(default_cfg);
            }
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
                    .map(|s| strip_comments(&s).contains("@SpringBootApplication"))
                    .unwrap_or(false)
        })
}

/// Strip single-line `//` and multi-line `/* */` comments from Java/Kotlin source,
/// respecting string literals.
pub(crate) fn strip_comments(code: &str) -> String {
    let mut res = String::with_capacity(code.len());
    let mut chars = code.chars().peekable();
    while let Some(c) = chars.next() {
        if c == '/' {
            if let Some(&'/') = chars.peek() {
                chars.next();
                for next_c in chars.by_ref() {
                    if next_c == '\n' {
                        res.push('\n');
                        break;
                    }
                }
            } else if let Some(&'*') = chars.peek() {
                chars.next();
                while let Some(next_c) = chars.next() {
                    if next_c == '\n' {
                        res.push('\n');
                    } else if next_c == '*' && chars.peek() == Some(&'/') {
                        chars.next();
                        res.push(' ');
                        break;
                    }
                }
            } else {
                res.push(c);
            }
        } else if c == '"' {
            res.push(c);
            let mut escape = false;
            for next_c in chars.by_ref() {
                res.push(next_c);
                if escape {
                    escape = false;
                } else if next_c == '\\' {
                    escape = true;
                } else if next_c == '"' {
                    break;
                }
            }
        } else if c == '\'' {
            res.push(c);
            let mut escape = false;
            for next_c in chars.by_ref() {
                res.push(next_c);
                if escape {
                    escape = false;
                } else if next_c == '\\' {
                    escape = true;
                } else if next_c == '\'' {
                    break;
                }
            }
        } else {
            res.push(c);
        }
    }
    res
}

/// Read the declared Java version from this pom or its parent poms.
/// Multi-module Maven projects usually declare `<java.version>` only in the root pom.
pub fn java_version_of(dir: &Path) -> Option<u32> {
    let mut cur = Some(dir);
    for _ in 0..=4 {
        let Some(d) = cur else { break };
        if let Ok(raw) = fs::read_to_string(d.join("pom.xml")) {
            let clean = strip_xml_comments(&raw);
            for tag in [
                "java.version",
                "maven.compiler.release",
                "maven.compiler.source",
            ] {
                if let Some(v) = xml_tag_value(&clean, tag)
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

fn strip_xml_comments(raw: &str) -> String {
    let mut out = String::with_capacity(raw.len());
    let mut rest = raw;
    while let Some(start) = rest.find("<!--") {
        out.push_str(&rest[..start]);
        let after = &rest[start + 4..];
        match after.find("-->") {
            Some(end) => rest = &after[end + 3..],
            None => return out,
        }
    }
    out.push_str(rest);
    out
}

fn pom_artifact_id(raw: &str) -> Option<String> {
    let mut reader = Reader::from_str(raw);
    reader.config_mut().trim_text(true);
    let mut depth = 0usize;
    let mut in_artifact_id = false;
    while let Ok(ev) = reader.read_event() {
        match ev {
            Event::Start(e) => {
                depth += 1;
                in_artifact_id = depth == 2 && e.name().as_ref() == "artifactId";
            }
            Event::Text(t) if in_artifact_id => {
                let v = t.as_ref().trim();
                if !v.is_empty() {
                    return Some(v.to_string());
                }
            }
            Event::End(_) => {
                in_artifact_id = false;
                depth = depth.saturating_sub(1);
            }
            Event::Eof => break,
            _ => {}
        }
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
            let code = strip_comments(&raw);
            if !code.contains("@SpringBootApplication") {
                continue;
            }
            let package = code
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
        idea_module: None,
        jre_name: None,
        vm_options: String::new(),
        program_args: String::new(),
        active_profiles: String::new(),
        origin: "auto".into(),
    }
}

fn idea_module_matches(idea_mod: &str, module_name: &str, artifact_id: Option<&str>) -> bool {
    let short = idea_mod.rsplit('.').next().unwrap_or(idea_mod);
    idea_mod.eq_ignore_ascii_case(module_name)
        || short.eq_ignore_ascii_case(module_name)
        || artifact_id
            .is_some_and(|a| a.eq_ignore_ascii_case(idea_mod) || a.eq_ignore_ascii_case(short))
}

fn config_matches_module(
    c: &LaunchConfig,
    module_name: &str,
    artifact_id: Option<&str>,
    main_class: Option<&str>,
    is_pom_packaging: bool,
) -> bool {
    if let Some(m) = c.idea_module.as_deref() {
        return idea_module_matches(m, module_name, artifact_id);
    }
    if is_pom_packaging {
        return false;
    }
    match (c.main_class.as_deref(), main_class) {
        (Some(cm), Some(pm)) => cm == pm,
        _ => false,
    }
}

fn config_matches_local_dir(
    c: &LaunchConfig,
    module_name: &str,
    artifact_id: Option<&str>,
    main_class: Option<&str>,
    is_pom_packaging: bool,
) -> bool {
    if let Some(m) = c.idea_module.as_deref() {
        return idea_module_matches(m, module_name, artifact_id);
    }
    if is_pom_packaging {
        return false;
    }
    match (c.main_class.as_deref(), main_class) {
        (Some(cm), Some(pm)) => cm == pm,
        // Standalone module without a detected main class still keeps its local configs.
        (_, None) | (None, _) => true,
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
    let mut is_build_tool_cfg = false;
    let mut has_boot_goal = false;
    let mut alt_jre_enabled: Option<bool> = None;
    let mut current: Option<LaunchConfig> = None;

    let commit = |cfg: Option<LaunchConfig>,
                  build_cfg: bool,
                  boot_goal: bool,
                  jre_enabled: Option<bool>,
                  list: &mut Vec<LaunchConfig>| {
        if let Some(mut c) = cfg {
            if build_cfg && !boot_goal {
                return;
            }
            if jre_enabled == Some(false) {
                c.jre_name = None;
            }
            if !list.iter().any(|x: &LaunchConfig| x.name == c.name) {
                list.push(c);
            }
        }
    };

    while let Ok(event) = reader.read_event() {
        // Both Start and Empty carry the same BytesStart payload; track self-closing
        // elements so a `<configuration ... />` is committed without waiting for End.
        let (e, is_empty) = match event {
            Event::Start(x) => (x, false),
            Event::Empty(x) => (x, true),
            Event::Text(t) => {
                if in_cfg && is_build_tool_cfg {
                    let s = t.as_ref();
                    if s.contains("spring-boot:run") || s.contains("bootRun") {
                        has_boot_goal = true;
                    }
                }
                continue;
            }
            Event::End(x) => {
                if x.name().as_ref() == "configuration" {
                    commit(
                        current.take(),
                        is_build_tool_cfg,
                        has_boot_goal,
                        alt_jre_enabled,
                        out,
                    );
                    in_cfg = false;
                    is_build_tool_cfg = false;
                    has_boot_goal = false;
                    alt_jre_enabled = None;
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
            // (often temporary=true). They are accepted here and bound to a module
            // via `<module name="..." />` or `MAIN_CLASS_NAME`.
            let is_app = ctype == "SpringBootApplicationConfigurationType"
                || ctype == "SpringApplicationConfigurationType"
                || ctype == "Application";
            let is_build = ctype == "MavenRunConfiguration" || ctype == "GradleRunConfiguration";
            if is_app || is_build {
                in_cfg = true;
                is_build_tool_cfg = is_build;
                has_boot_goal = false;
                alt_jre_enabled = None;
                let c = LaunchConfig {
                    name: attr(&e, "name").unwrap_or_else(|| "Spring Boot".into()),
                    main_class: attr(&e, "MAIN_CLASS_NAME"),
                    module_dir: None,
                    idea_module: None,
                    jre_name: None,
                    vm_options: String::new(),
                    program_args: String::new(),
                    active_profiles: String::new(),
                    origin: "IDEA run configuration".into(),
                };
                if is_empty {
                    commit(
                        Some(c),
                        is_build_tool_cfg,
                        has_boot_goal,
                        alt_jre_enabled,
                        out,
                    );
                    in_cfg = false;
                    is_build_tool_cfg = false;
                } else {
                    current = Some(c);
                }
            }
        } else if in_cfg && tag == "module" {
            if let Some(c) = current.as_mut()
                && let Some(name) = attr(&e, "name").filter(|s| !s.is_empty())
            {
                c.idea_module = Some(name);
            }
        } else if in_cfg && tag == "option" {
            let key = attr(&e, "name").unwrap_or_default();
            let val = attr(&e, "value").unwrap_or_default();
            if is_build_tool_cfg && (val.contains("spring-boot:run") || val.contains("bootRun")) {
                has_boot_goal = true;
            }
            if let Some(c) = current.as_mut() {
                match key.as_str() {
                    "MAIN_CLASS_NAME" | "SPRING_BOOT_MAIN_CLASS" if !val.is_empty() => {
                        c.main_class = Some(val);
                    }
                    "VM_PARAMETERS" => c.vm_options = val,
                    "PROGRAM_PARAMETERS" => c.program_args = val,
                    "ACTIVE_PROFILES" => c.active_profiles = val,
                    "ALTERNATIVE_JRE_PATH" if !val.is_empty() => {
                        c.jre_name = Some(val);
                    }
                    "ALTERNATIVE_JRE_PATH_ENABLED" => {
                        alt_jre_enabled = Some(val.eq_ignore_ascii_case("true"));
                    }
                    _ => {}
                }
            }
        }
    }
    commit(
        current.take(),
        is_build_tool_cfg,
        has_boot_goal,
        alt_jre_enabled,
        out,
    );
}

fn attr(e: &quick_xml::events::BytesStart<'_>, key: &str) -> Option<String> {
    e.attributes()
        .flatten()
        .find(|a| a.key.as_ref().eq_ignore_ascii_case(key))
        .map(|a| crate::scan::unescape_xml(a.value.as_ref()))
}

/// Read up to `max_lines` from the tail of a log file, reading at most the last 2MB.
pub fn read_file_tail(path: &Path, max_lines: usize) -> std::io::Result<Vec<String>> {
    use std::fs::File;
    use std::io::{Read, Seek, SeekFrom};

    let mut file = File::open(path)?;
    let metadata = file.metadata()?;
    let len = metadata.len();
    if len == 0 {
        return Ok(Vec::new());
    }

    const MAX_BYTES: u64 = 2 * 1024 * 1024;
    let seek_pos = len.saturating_sub(MAX_BYTES);
    if seek_pos > 0 {
        file.seek(SeekFrom::Start(seek_pos))?;
    }

    let mut buf = Vec::new();
    file.read_to_end(&mut buf)?;

    let text = String::from_utf8_lossy(&buf);
    let mut lines: Vec<&str> = text.lines().collect();

    if seek_pos > 0 && !lines.is_empty() {
        lines.remove(0);
    }

    let count = lines.len();
    let skip = count.saturating_sub(max_lines);
    Ok(lines.into_iter().skip(skip).map(String::from).collect())
}

/// Read `spring.application.name` from `application.{yml,yaml,properties}` under `project_dir`.
pub fn spring_app_name(project_dir: &Path) -> Option<String> {
    let mut dirs = Vec::new();
    let mut config_prefixes = Vec::new();
    let mut app_prefixes = Vec::new();
    collect_spring_configs(
        project_dir,
        &mut dirs,
        &mut config_prefixes,
        &mut app_prefixes,
        false,
    );
    app_prefixes.into_iter().next()
}

fn collect_spring_configs(
    p: &Path,
    dirs: &mut Vec<PathBuf>,
    config_prefixes: &mut Vec<String>,
    app_prefixes: &mut Vec<String>,
    include_xml: bool,
) {
    let res_dir = p.join("src").join("main").join("resources");
    for dir in [&res_dir, p] {
        if !dir.is_dir() {
            continue;
        }
        if include_xml {
            for name in [
                "logback-spring.xml",
                "logback.xml",
                "log4j2-spring.xml",
                "log4j2.xml",
            ] {
                let xml_file = dir.join(name);
                if let Ok(raw) = fs::read_to_string(&xml_file) {
                    extract_logback_info(&raw, p, dirs, config_prefixes);
                }
            }
        }
        for (name, is_props) in [
            ("application.yml", false),
            ("application.yaml", false),
            ("application.properties", true),
        ] {
            let cfg_file = dir.join(name);
            if let Ok(raw) = fs::read_to_string(&cfg_file) {
                extract_spring_config_log_info(
                    &raw,
                    is_props,
                    p,
                    dirs,
                    config_prefixes,
                    app_prefixes,
                );
            }
        }
    }
}

/// Discover the most relevant active log file for a running application or project.
pub fn find_log_file(
    project_path: Option<&Path>,
    project_name: Option<&str>,
    main_class: Option<&str>,
    app_name: Option<&str>,
    work_dir: Option<&Path>,
    cmdline: Option<&str>,
    scan_roots: &[PathBuf],
) -> Option<PathBuf> {
    let mut dirs: Vec<PathBuf> = Vec::new();
    // Explicit log file stems from logback / logging.file.name / -Dlogging.file.name.
    let mut config_prefixes: Vec<String> = Vec::new();
    // Service / project / app name prefixes.
    let mut app_prefixes: Vec<String> = Vec::new();

    // 1. Check project configuration files (logback, log4j2, application.yml/.properties)
    if let Some(p) = project_path {
        collect_spring_configs(p, &mut dirs, &mut config_prefixes, &mut app_prefixes, true);
    }

    // 2. JVM command line arguments (e.g. -Dlogging.file.name, -DLOG_HOME)
    if let Some(cmd) = cmdline {
        extract_cmdline_log_info(cmd, project_path, &mut dirs, &mut config_prefixes);
    }

    // 3. Conventional directory locations
    if let Some(p) = project_path {
        dirs.push(p.join("logs"));
        dirs.push(p.join("log"));
        dirs.push(p.to_path_buf());
        for ancestor in p.ancestors().skip(1).take(3) {
            dirs.push(ancestor.join("logs"));
            dirs.push(ancestor.join("log"));
        }
    }
    if let Some(w) = work_dir {
        dirs.push(w.join("logs"));
        dirs.push(w.join("log"));
        dirs.push(w.to_path_buf());
    }
    for root in scan_roots {
        dirs.push(root.join("logs"));
        dirs.push(root.join("log"));
    }

    // Collect name tokens
    let mut tokens: Vec<String> = Vec::new();
    if let Some(mc) = main_class {
        let short = mc.rsplit('.').next().unwrap_or(mc);
        for w in split_words(short) {
            if w.len() >= 3 && !tokens.contains(&w) {
                tokens.push(w);
            }
        }
    }
    if let Some(pn) = project_name {
        for w in split_words(pn) {
            if w.len() >= 3 && !tokens.contains(&w) {
                tokens.push(w);
            }
        }
        push_unique_lower(&mut app_prefixes, pn);
    }
    if let Some(an) = app_name {
        for w in split_words(an) {
            if w.len() >= 3 && !tokens.contains(&w) {
                tokens.push(w);
            }
        }
        push_unique_lower(&mut app_prefixes, an);
    }

    let canon_project = project_path.and_then(|p| p.canonicalize().ok());
    // Filter and deduplicate existing candidate directories
    let mut checked_dirs: HashSet<PathBuf> = HashSet::new();
    let mut valid_dirs: Vec<(PathBuf, bool)> = Vec::new();
    for d in dirs {
        let resolved = d.canonicalize().ok();
        let dir_path = resolved.clone().unwrap_or(d);
        if dir_path.is_dir() && checked_dirs.insert(dir_path.clone()) {
            let exclusive = canon_project
                .as_ref()
                .is_some_and(|cp| dir_path.starts_with(cp));
            valid_dirs.push((dir_path, exclusive));
        }
    }

    // Scan candidate directories for log files
    struct Candidate {
        path: PathBuf,
        score: i64,
        modified: std::time::SystemTime,
    }
    let mut candidates: Vec<Candidate> = Vec::new();
    let crit = LogCriteria {
        config_prefixes: &config_prefixes,
        app_prefixes: &app_prefixes,
        tokens: &tokens,
    };

    for (d, exclusive) in valid_dirs {
        let Ok(entries) = fs::read_dir(&d) else {
            continue;
        };
        for entry in entries.flatten() {
            let path = entry.path();
            if path.is_file() {
                if let Some(cand) = score_log_file(&path, &entry, &crit, exclusive) {
                    candidates.push(Candidate {
                        path: cand.0,
                        score: cand.1,
                        modified: cand.2,
                    });
                }
            } else if path.is_dir() {
                // Check 1 level of subdirectories if folder name relates to app/logs
                let dname = path
                    .file_name()
                    .unwrap_or_default()
                    .to_string_lossy()
                    .to_lowercase();
                let matches_dir = tokens.iter().any(|t| dname.contains(t))
                    || config_prefixes.iter().any(|p| dname.contains(p))
                    || app_prefixes.iter().any(|p| dname.contains(p));
                if matches_dir && let Ok(sub_entries) = fs::read_dir(&path) {
                    for sub in sub_entries.flatten() {
                        let sub_path = sub.path();
                        if sub_path.is_file()
                            && let Some(cand) = score_log_file(&sub_path, &sub, &crit, exclusive)
                        {
                            candidates.push(Candidate {
                                path: cand.0,
                                score: cand.1,
                                modified: cand.2,
                            });
                        }
                    }
                }
            }
        }
    }

    candidates.sort_by(|a, b| {
        b.score
            .cmp(&a.score)
            .then_with(|| b.modified.cmp(&a.modified))
    });
    candidates
        .first()
        .filter(|c| c.score > 0)
        .map(|c| crate::scan::strip_unc_prefix(&c.path))
}

struct LogCriteria<'a> {
    config_prefixes: &'a [String],
    app_prefixes: &'a [String],
    tokens: &'a [String],
}

fn is_generic_token(token: &str) -> bool {
    ["application", "start", "server", "boot", "service", "main"].contains(&token)
}

fn push_unique_lower(list: &mut Vec<String>, val: &str) {
    let lower = val.trim().to_lowercase();
    if !lower.is_empty() && !list.contains(&lower) {
        list.push(lower);
    }
}

fn score_log_file(
    path: &Path,
    entry: &fs::DirEntry,
    crit: &LogCriteria<'_>,
    dir_exclusive: bool,
) -> Option<(PathBuf, i64, std::time::SystemTime)> {
    let fname = path.file_name()?.to_string_lossy().to_lowercase();
    if !fname.ends_with(".log")
        && !fname.ends_with(".out")
        && !fname.ends_with(".txt")
        && !fname.contains(".log.")
    {
        return None;
    }
    let mut score = 0i64;
    let mut strong_match = false;

    if crit
        .config_prefixes
        .iter()
        .any(|p| !p.is_empty() && fname.contains(p))
    {
        score += 200;
        strong_match = true;
    }

    if crit
        .app_prefixes
        .iter()
        .any(|p| !p.is_empty() && fname.contains(p))
    {
        score += 120;
        strong_match = true;
    }

    let specific_total = crit
        .tokens
        .iter()
        .filter(|t| !is_generic_token(t.as_str()))
        .count();
    let mut specific_hits = 0usize;
    for token in crit.tokens {
        if fname.contains(token) {
            if is_generic_token(token.as_str()) {
                score += 5;
            } else {
                specific_hits += 1;
                score += 25;
            }
        }
    }
    // Without an explicit log config prefix, require strong multi-token agreement
    // (or all specific tokens if 1) so shared prefixes like "jeecg" do not match sibling services.
    if crit.config_prefixes.is_empty()
        && specific_hits > 0
        && specific_hits >= specific_total.min(2)
    {
        strong_match = true;
    }

    // Never pick an unmatched log file out of a shared ../logs directory.
    if !strong_match && !dir_exclusive {
        return None;
    }

    let meta = entry.metadata().ok()?;
    let len = meta.len();
    if len == 0 {
        score -= 20;
    } else {
        score += 10;
    }

    let modified = meta.modified().unwrap_or(std::time::SystemTime::UNIX_EPOCH);
    if let Ok(elapsed) = modified.elapsed() {
        let secs = elapsed.as_secs();
        if secs < 600 {
            score += 50;
        } else if secs < 3600 * 2 {
            score += 35;
        } else if secs < 3600 * 24 {
            score += 20;
        } else if secs < 3600 * 24 * 7 {
            score += 0;
        } else {
            score -= 10;
        }
    }

    Some((path.to_path_buf(), score, modified))
}

fn split_words(s: &str) -> Vec<String> {
    let mut words = Vec::new();
    let mut current = String::new();
    for c in s.chars() {
        if c == '-' || c == '_' || c == '.' || c == '/' || c == '\\' {
            if !current.is_empty() {
                words.push(current.to_lowercase());
                current.clear();
            }
        } else if c.is_uppercase()
            && !current.is_empty()
            && current.chars().last().is_some_and(|l| l.is_lowercase())
        {
            words.push(current.to_lowercase());
            current.clear();
            current.push(c);
        } else {
            current.push(c);
        }
    }
    if !current.is_empty() {
        words.push(current.to_lowercase());
    }
    words
}

fn expand_home_placeholders(raw: &str) -> String {
    let home = dirs::home_dir().unwrap_or_default();
    let home_str = home.to_string_lossy();
    raw.replace("${user.home}", &home_str)
        .replace("${USER_HOME}", &home_str)
}

fn resolve_log_path(raw: &str, base: Option<&Path>) -> PathBuf {
    let expanded = expand_home_placeholders(raw);
    let p = Path::new(&expanded);
    match (p.is_relative(), base) {
        (true, Some(b)) => b.join(p),
        _ => p.to_path_buf(),
    }
}

fn is_non_text_log_pattern(name_lower: &str) -> bool {
    name_lower.ends_with(".html")
        || name_lower.ends_with(".htm")
        || name_lower.ends_with(".gz")
        || name_lower.ends_with(".zip")
}

fn extract_logback_info(
    raw: &str,
    project_dir: &Path,
    dirs: &mut Vec<PathBuf>,
    prefixes: &mut Vec<String>,
) {
    let mut reader = Reader::from_str(raw);
    reader.config_mut().trim_text(true);
    let mut properties: std::collections::HashMap<String, String> =
        std::collections::HashMap::new();
    let mut current_tag = String::new();

    while let Ok(event) = reader.read_event() {
        match event {
            Event::Start(e) | Event::Empty(e) => {
                let qname = e.name();
                let tag = qname.as_ref();
                if tag.eq_ignore_ascii_case("property") {
                    let name = attr(&e, "name").unwrap_or_default();
                    let mut val = expand_home_placeholders(&attr(&e, "value").unwrap_or_default());
                    if !name.is_empty() && !val.is_empty() {
                        for (pk, pv) in &properties {
                            let placeholder = format!("${{{pk}}}");
                            if val.contains(&placeholder) {
                                val = val.replace(&placeholder, pv);
                            }
                        }
                        properties.insert(name.clone(), val.clone());
                        let name_lower = name.to_lowercase();
                        if name_lower.contains("log")
                            || name_lower.contains("dir")
                            || name_lower.contains("path")
                        {
                            dirs.push(resolve_log_path(&val, Some(project_dir)));
                        }
                    }
                }
                current_tag = tag.to_string();
            }
            Event::Text(t) => {
                let tag_lower = current_tag.to_lowercase();
                if tag_lower == "file" || tag_lower.contains("filenamepattern") {
                    let mut text = expand_home_placeholders(t.as_ref());
                    for (pk, pv) in &properties {
                        let placeholder = format!("${{{pk}}}");
                        if text.contains(&placeholder) {
                            text = text.replace(&placeholder, pv);
                        }
                    }
                    if !text.is_empty() {
                        let resolved = resolve_log_path(&text, Some(project_dir));
                        let fname_lower = resolved
                            .file_name()
                            .and_then(|f| f.to_str())
                            .unwrap_or("")
                            .to_lowercase();
                        if is_non_text_log_pattern(&fname_lower) {
                            continue;
                        }
                        if let Some(parent) = resolved.parent()
                            && !parent.as_os_str().is_empty()
                        {
                            dirs.push(parent.to_path_buf());
                        }
                        if let Some(fname) = resolved.file_name().and_then(|f| f.to_str()) {
                            let clean = fname
                                .split('%')
                                .next()
                                .unwrap_or(fname)
                                .trim_end_matches(".log")
                                .trim_end_matches(".out")
                                .trim_end_matches(".txt")
                                .trim_matches(['-', '_', '.']);
                            if clean.len() >= 3 {
                                push_unique_lower(prefixes, clean);
                            }
                        }
                    }
                }
            }
            Event::End(_) => {
                current_tag.clear();
            }
            Event::Eof => break,
            _ => {}
        }
    }
}

fn strip_inline_comment(line: &str) -> &str {
    let mut in_single = false;
    let mut in_double = false;
    let bytes = line.as_bytes();
    for (i, &b) in bytes.iter().enumerate() {
        match b {
            b'\'' if !in_double => in_single = !in_single,
            b'"' if !in_single => in_double = !in_double,
            b'#' if !in_single && !in_double && (i == 0 || bytes[i - 1].is_ascii_whitespace()) => {
                return &line[..i];
            }
            _ => {}
        }
    }
    line
}

fn record_spring_log_kv(
    key: &str,
    raw_val: &str,
    project_dir: &Path,
    dirs: &mut Vec<PathBuf>,
    config_prefixes: &mut Vec<String>,
    app_prefixes: &mut Vec<String>,
) {
    let val = raw_val.trim().trim_matches(['"', '\'']);
    if val.is_empty() {
        return;
    }
    if key == "spring.application.name" {
        push_unique_lower(app_prefixes, val);
    } else if key == "logging.file.name" || key == "logging.file" {
        let resolved = resolve_log_path(val, Some(project_dir));
        if let Some(parent) = resolved.parent()
            && !parent.as_os_str().is_empty()
        {
            dirs.push(parent.to_path_buf());
        }
        if let Some(stem) = resolved.file_stem().and_then(|s| s.to_str()) {
            push_unique_lower(config_prefixes, stem);
        }
    } else if key == "logging.file.path" || key == "logging.path" || key == "log.path" {
        dirs.push(resolve_log_path(val, Some(project_dir)));
    }
}

fn extract_spring_config_log_info(
    raw: &str,
    is_properties: bool,
    project_dir: &Path,
    dirs: &mut Vec<PathBuf>,
    config_prefixes: &mut Vec<String>,
    app_prefixes: &mut Vec<String>,
) {
    if is_properties {
        for line in raw.lines() {
            let trimmed = line.trim();
            if trimmed.is_empty() || trimmed.starts_with('#') || trimmed.starts_with('!') {
                continue;
            }
            let Some((k, v)) = trimmed.split_once('=').or_else(|| trimmed.split_once(':')) else {
                continue;
            };
            record_spring_log_kv(
                k.trim(),
                v,
                project_dir,
                dirs,
                config_prefixes,
                app_prefixes,
            );
        }
        return;
    }

    // Track indentation stack so nested YAML keys (spring.application.name,
    // logging.file.name, logging.file.path) resolve cleanly without a YAML crate.
    let mut stack: Vec<(usize, String)> = Vec::new();
    for line in raw.lines() {
        let clean = strip_inline_comment(line);
        let trimmed = clean.trim();
        if trimmed.is_empty() || trimmed == "---" || trimmed.starts_with('-') {
            continue;
        }
        let indent = clean
            .chars()
            .take_while(|c| *c == ' ' || *c == '\t')
            .map(|c| if c == '\t' { 2 } else { 1 })
            .sum::<usize>();
        let Some((raw_k, raw_v)) = trimmed.split_once(':') else {
            continue;
        };
        let k = raw_k.trim().trim_matches(['"', '\'']);
        if k.is_empty() {
            continue;
        }
        while stack.last().is_some_and(|(ind, _)| *ind >= indent) {
            stack.pop();
        }
        let v = raw_v.trim();
        if v.is_empty() || v == "|" || v == ">" {
            stack.push((indent, k.to_string()));
            continue;
        }
        let full_key: String = stack
            .iter()
            .map(|(_, seg)| seg.as_str())
            .chain(std::iter::once(k))
            .collect::<Vec<_>>()
            .join(".");
        record_spring_log_kv(
            &full_key,
            v,
            project_dir,
            dirs,
            config_prefixes,
            app_prefixes,
        );
    }
}

fn extract_cmdline_log_info(
    cmd: &str,
    project_dir: Option<&Path>,
    dirs: &mut Vec<PathBuf>,
    prefixes: &mut Vec<String>,
) {
    for token in cmd.split_whitespace() {
        let clean = token.trim_matches('"').trim_matches('\'');
        for key in [
            "-Dlogging.file.name=",
            "-Dlogging.file=",
            "-DLOG_PATH=",
            "-DLOG_HOME=",
            "-Dlogging.file.path=",
        ] {
            if let Some(val) = clean.strip_prefix(key) {
                let resolved = resolve_log_path(val, project_dir);
                if val.ends_with(".log") || val.ends_with(".out") || val.ends_with(".txt") {
                    if let Some(parent) = resolved.parent()
                        && !parent.as_os_str().is_empty()
                    {
                        dirs.push(parent.to_path_buf());
                    }
                    if let Some(stem) = resolved.file_stem().and_then(|s| s.to_str()) {
                        push_unique_lower(prefixes, stem);
                    }
                } else {
                    dirs.push(resolved);
                }
            }
        }
    }
}
