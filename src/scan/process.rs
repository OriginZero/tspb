use crate::model::{Origin, RunningApp};
use crate::progress::Reporter;
use crate::scan::env::home_from_java_exe;
use std::path::PathBuf;
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
        let jdk_home = cmd
            .first()
            .and_then(|s| home_from_java_exe(&PathBuf::from(s.trim_matches('"'))));
        let work_dir = extract_work_dir(&cmd);
        let ports = extract_ports(&joined);

        out.push(RunningApp {
            pid: pid.as_u32(),
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

fn extract_work_dir(cmd: &[String]) -> Option<PathBuf> {
    for a in cmd {
        if let Some(v) = a.strip_prefix("-Duser.dir=") {
            return Some(PathBuf::from(v));
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
