use crate::model::{Origin, RunningApp};
use crate::progress::Reporter;
use crate::scan::env::home_from_java_exe;
use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::{Mutex, OnceLock};
use std::time::{Duration, Instant};
use sysinfo::{ProcessRefreshKind, ProcessesToUpdate, System, UpdateKind};

/// Java processes that are NOT Spring Boot applications (IDE helpers, build daemons).
const NON_APP_MARKERS: &[&str] = &[
    "org.jetbrains.idea.maven.server",
    "RemoteMavenServer",
    "org.gradle.launcher.daemon",
    "GradleDaemon",
    "intellij.maven.server",
    "maven-server.jar",
    "org.codehaus.plexus.classworlds.launcher.Launcher",
    "jdk.jcmd",
    "sun.tools.jps.Jps",
    "com.intellij.rt",
    "org.apache.maven",
    "kotlin.daemon",
];

pub fn scan() -> Vec<RunningApp> {
    scan_tracked(&Reporter::silent())
}

pub fn scan_tracked(rep: &Reporter) -> Vec<RunningApp> {
    rep.detail("enumerating java processes");
    // refresh_processes() skips command lines; without cmd() every Java process
    // would be invisible. Request only cmd so the 3s UI refresh stays cheap.
    let mut sys = System::new();
    sys.refresh_processes_specifics(
        ProcessesToUpdate::All,
        true,
        ProcessRefreshKind::nothing().with_cmd(UpdateKind::OnlyIfNotSet),
    );
    let mut out = Vec::new();

    for (pid, proc) in sys.processes() {
        let pname = proc.name().to_string_lossy().to_lowercase();
        if !pname.starts_with("java") || pname.contains("javaw") {
            continue;
        }
        let cmd: Vec<String> = proc
            .cmd()
            .iter()
            .map(|a| a.to_string_lossy().into_owned())
            .collect();
        if cmd.is_empty()
            || NON_APP_MARKERS
                .iter()
                .any(|m| cmd.iter().any(|a| a.contains(m)))
        {
            continue;
        }
        let joined = cmd.join(" ");
        let Some((main_class, jar_name)) = extract_main_class(&cmd) else {
            continue;
        };
        let name = match jar_name {
            Some(j) => j,
            None => main_class
                .rsplit('.')
                .next()
                .unwrap_or(&main_class)
                .to_string(),
        };
        let (ancestor_pids, mvn_dir) = collect_ancestors_and_mvn_dir(&sys, *pid);
        let jdk_home = cmd
            .first()
            .and_then(|s| home_from_java_exe(&PathBuf::from(s.trim_matches('"'))));
        let work_dir = extract_work_dir(&cmd)
            .or(mvn_dir)
            .or_else(|| extract_work_dir_from_argfile(&cmd));
        let ports = extract_ports(&joined);

        out.push(RunningApp {
            pid: pid.as_u32(),
            ancestor_pids,
            main_class,
            name,
            jdk_home,
            work_dir,
            ports,
            cmdline: joined,
            managed: false,
            origin: Origin::System,
        });
    }
    out.sort_by_key(|a| a.name.to_lowercase());
    enrich_listening_ports(&mut out);
    rep.running(out.len());
    out
}

/// Returns (main class or jar stem, optional jar label for `-jar` launches).
pub(crate) fn extract_main_class(cmd: &[String]) -> Option<(String, Option<String>)> {
    // Spring Boot fat jar / war launchers (SB2 and SB3.2+).
    for launcher in [
        "org.springframework.boot.loader.JarLauncher",
        "org.springframework.boot.loader.PropertiesLauncher",
        "org.springframework.boot.loader.WarLauncher",
        "org.springframework.boot.loader.launch.JarLauncher",
        "org.springframework.boot.loader.launch.PropertiesLauncher",
        "org.springframework.boot.loader.launch.WarLauncher",
    ] {
        if cmd.iter().any(|a| a == launcher) {
            let jar = cmd
                .windows(2)
                .find(|w| w[0] == "-jar" || w[0] == "-cp" || w[0] == "-classpath")
                .and_then(|w| {
                    let p = PathBuf::from(&w[1]);
                    p.file_stem().map(|s| s.to_string_lossy().into_owned())
                });
            return Some((launcher.to_string(), jar));
        }
    }

    // `java [opts] -jar app.jar [args...]`
    if let Some(w) = cmd.windows(2).find(|w| w[0] == "-jar") {
        let p = PathBuf::from(&w[1]);
        if let Some(stem) = p.file_stem().map(|s| s.to_string_lossy().into_owned())
            && !stem.is_empty()
        {
            return Some((stem.clone(), Some(stem)));
        }
    }

    // Plain `java [opts] com.example.DemoApplication [args...]` (typical IDEA run configuration).
    let mut skip_next = false;
    for arg in cmd.iter().skip(1) {
        if skip_next {
            skip_next = false;
            continue;
        }
        if [
            "-cp",
            "-classpath",
            "--class-path",
            "-p",
            "--module-path",
            "-m",
            "--module",
            "--add-opens",
            "--add-exports",
            "--add-reads",
            "--patch-module",
            "--upgrade-module-path",
        ]
        .contains(&arg.as_str())
        {
            skip_next = true;
            continue;
        }
        if arg.starts_with('-') || arg.starts_with('@') {
            continue;
        }
        // First positional argument after JVM options is the main class; require a dotted package name.
        if arg.contains('.')
            && !arg.ends_with(".jar")
            && !arg.contains('\\')
            && !arg.contains('/')
            && !arg.contains('=')
            && arg.chars().next().is_some_and(|c| c.is_ascii_lowercase())
        {
            return Some((arg.clone(), None));
        }
    }
    None
}

fn collect_ancestors_and_mvn_dir(
    sys: &System,
    start_pid: sysinfo::Pid,
) -> (Vec<u32>, Option<PathBuf>) {
    let mut ancestors = Vec::new();
    let mut mvn_dir = None;
    let mut cur = start_pid;
    for _ in 0..8 {
        let Some(ppid) = sys.process(cur).and_then(|p| p.parent()) else {
            break;
        };
        let raw_id = ppid.as_u32();
        if raw_id == 0 || ancestors.contains(&raw_id) {
            break;
        }
        ancestors.push(raw_id);
        if mvn_dir.is_none()
            && let Some(parent) = sys.process(ppid)
        {
            for arg in parent.cmd() {
                let s = arg.to_string_lossy();
                let clean = s.trim_matches('"').trim_matches('\'');
                if let Some(dir) = clean.strip_prefix("-Dmaven.multiModuleProjectDirectory=")
                    && !dir.is_empty()
                {
                    mvn_dir = Some(PathBuf::from(dir));
                    break;
                }
            }
        }
        cur = ppid;
    }
    (ancestors, mvn_dir)
}

fn extract_work_dir(cmd: &[String]) -> Option<PathBuf> {
    for a in cmd {
        if let Some(v) = a.strip_prefix("-Duser.dir=") {
            return Some(PathBuf::from(v));
        }
    }
    None
}

/// Spring Boot Maven plugin and IDEA shorten long classpaths via `@argfile`.
/// The first entry is the module's own `target/classes` (or `build/classes/...`),
/// which identifies the exact module even when multiple modules share a main class.
pub(crate) fn extract_work_dir_from_argfile(cmd: &[String]) -> Option<PathBuf> {
    use std::io::Read;

    let argfile = cmd
        .iter()
        .find_map(|a| a.trim_matches('"').strip_prefix('@'))?;
    let mut file = std::fs::File::open(argfile).ok()?;
    let mut buf = vec![0u8; 8192];
    let n = file.read(&mut buf).ok()?;
    let raw = String::from_utf8_lossy(&buf[..n]);
    parse_module_dir_from_classpath(&raw)
}

pub(crate) fn parse_module_dir_from_classpath(raw: &str) -> Option<PathBuf> {
    let unescaped = raw.replace("\\\\", "\\").replace('"', "");
    let sep_split = unescaped.split(|c: char| {
        c == ';' || c == '\n' || c == '\r' || c == '\t' || c == ' ' || (!cfg!(windows) && c == ':')
    });
    for entry in sep_split {
        let norm = entry.trim().replace('\\', "/");
        for suffix in [
            "/target/classes",
            "/build/classes/java/main",
            "/build/classes/kotlin/main",
        ] {
            if let Some(base) = norm.strip_suffix(suffix)
                && !base.is_empty()
            {
                let native = if cfg!(windows) {
                    base.replace('/', "\\")
                } else {
                    base.to_string()
                };
                return Some(PathBuf::from(native));
            }
        }
    }
    None
}

fn extract_ports(joined: &str) -> Vec<u16> {
    let mut ports = Vec::new();
    for token in joined.split_whitespace() {
        for key in ["--server.port=", "-Dserver.port=", "--port="] {
            if let Some(v) = token.trim_matches('"').strip_prefix(key)
                && let Ok(p) = v.parse::<u16>()
                && !ports.contains(&p)
            {
                ports.push(p);
            }
        }
    }
    ports
}

struct ListeningPortCache {
    fetched_at: Instant,
    ports: HashMap<u32, Vec<u16>>,
}

static LISTENING_PORTS: OnceLock<Mutex<ListeningPortCache>> = OnceLock::new();

fn enrich_listening_ports(apps: &mut [RunningApp]) {
    if apps.is_empty() {
        return;
    }
    let cache_lock = LISTENING_PORTS.get_or_init(|| {
        Mutex::new(ListeningPortCache {
            fetched_at: Instant::now() - Duration::from_secs(60),
            ports: HashMap::new(),
        })
    });
    let Ok(mut cache) = cache_lock.lock() else {
        return;
    };
    let has_unseen = apps.iter().any(|a| !cache.ports.contains_key(&a.pid));
    let any_empty = apps
        .iter()
        .any(|a| a.ports.is_empty() && cache.ports.get(&a.pid).is_none_or(Vec::is_empty));
    let ttl = if any_empty {
        Duration::from_secs(3)
    } else {
        Duration::from_secs(15)
    };
    if has_unseen || cache.fetched_at.elapsed() >= ttl {
        cache.ports = collect_listening_ports();
        cache.fetched_at = Instant::now();
    }
    for app in apps {
        if let Some(os_ports) = cache.ports.get(&app.pid) {
            for &p in os_ports {
                if !app.ports.contains(&p) {
                    app.ports.push(p);
                }
            }
        }
    }
}

fn collect_listening_ports() -> HashMap<u32, Vec<u16>> {
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        use std::process::Command;

        const CREATE_NO_WINDOW: u32 = 0x0800_0000;
        let Ok(out) = Command::new("netstat")
            .args(["-ano", "-p", "TCP"])
            .creation_flags(CREATE_NO_WINDOW)
            .output()
        else {
            return HashMap::new();
        };
        let raw = String::from_utf8_lossy(&out.stdout);
        parse_netstat_listening(&raw)
    }
    #[cfg(unix)]
    {
        use std::process::Command;

        if let Ok(out) = Command::new("lsof")
            .args(["-nP", "-iTCP", "-sTCP:LISTEN", "-Fn", "-Fp"])
            .output()
            && out.status.success()
        {
            return parse_lsof_listening(&String::from_utf8_lossy(&out.stdout));
        }
        HashMap::new()
    }
}

pub(crate) fn parse_netstat_listening(raw: &str) -> HashMap<u32, Vec<u16>> {
    let mut map: HashMap<u32, Vec<u16>> = HashMap::new();
    for line in raw.lines() {
        let cols: Vec<&str> = line.split_whitespace().collect();
        if cols.len() < 5 || !cols[0].eq_ignore_ascii_case("TCP") {
            continue;
        }
        let foreign_unbound = cols[2] == "0.0.0.0:0" || cols[2] == "[::]:0";
        let state_listen = cols[3].eq_ignore_ascii_case("LISTENING")
            || cols[3].eq_ignore_ascii_case("LISTEN")
            || cols[3] == "侦听";
        if !foreign_unbound && !state_listen {
            continue;
        }
        let Some((_, port_str)) = cols[1].rsplit_once(':') else {
            continue;
        };
        let (Ok(port), Ok(pid)) = (port_str.parse::<u16>(), cols[4].parse::<u32>()) else {
            continue;
        };
        if port == 0 || pid == 0 {
            continue;
        }
        let entry = map.entry(pid).or_default();
        if !entry.contains(&port) {
            entry.push(port);
        }
    }
    for ports in map.values_mut() {
        ports.sort_unstable();
    }
    map
}

#[cfg(unix)]
fn parse_lsof_listening(raw: &str) -> HashMap<u32, Vec<u16>> {
    let mut map: HashMap<u32, Vec<u16>> = HashMap::new();
    let mut cur_pid = None;
    for line in raw.lines() {
        if let Some(rest) = line.strip_prefix('p') {
            cur_pid = rest.trim().parse::<u32>().ok();
        } else if let Some(rest) = line.strip_prefix('n')
            && let Some(pid) = cur_pid
            && let Some((_, port_str)) = rest.rsplit_once(':')
            && let Ok(port) = port_str.trim().parse::<u16>()
            && port > 0
        {
            let entry = map.entry(pid).or_default();
            if !entry.contains(&port) {
                entry.push(port);
            }
        }
    }
    for ports in map.values_mut() {
        ports.sort_unstable();
    }
    map
}
