use crate::config::Config as AppConfig;
use crate::model::{Jdk, LaunchConfig, Maven, Project, RunState};
use anyhow::{Context, Result};
use std::collections::{HashMap, VecDeque};
use std::io::{BufRead, BufReader, Read};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

#[cfg(unix)]
use std::os::unix::process::CommandExt;

pub type LogBuf = Arc<Mutex<VecDeque<String>>>;

pub struct Managed {
    pub id: String,
    pub label: String,
    pub pid: u32,
    pub state: RunState,
    pub project_path: PathBuf,
    pub started_at: String,
    pub logs: LogBuf,
    started_flag: Arc<AtomicBool>,
    active_readers: Arc<AtomicUsize>,
    exited_at: Option<Instant>,
    child: Child,
}

#[derive(Default)]
pub struct Runner {
    pub managed: HashMap<String, Managed>,
    capacity: usize,
}

impl Runner {
    pub fn new(capacity: usize) -> Self {
        Self {
            managed: HashMap::new(),
            capacity,
        }
    }

    pub fn launch(
        &mut self,
        project: &Project,
        cfg: &LaunchConfig,
        jdk: &Jdk,
        maven: Option<&Maven>,
        app_cfg: &AppConfig,
    ) -> Result<String> {
        let id = format!("{}::{}", project.path.display(), cfg.name);
        if let Some(m) = self.managed.get(&id) {
            if m.state == RunState::Running || m.state == RunState::Starting {
                anyhow::bail!("already running: {}", m.label);
            }
            self.managed.remove(&id);
        }

        let cwd = cfg
            .module_dir
            .clone()
            .unwrap_or_else(|| project.path.clone());
        let (program, args) = build_invocation(project, cfg, maven, app_cfg);
        let mut command = wrap_command(&program, &cwd);

        command
            .args(&args)
            .current_dir(&cwd)
            .env("JAVA_HOME", &jdk.home)
            .env("PATH", prepend_path(&jdk.home.join("bin")))
            .env_remove("JAVA_TOOL_OPTIONS")
            .env_remove("_JAVA_OPTIONS")
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        if let Some(m) = maven {
            command.env("MAVEN_HOME", &m.home).env("M2_HOME", &m.home);
        }
        #[cfg(unix)]
        command.process_group(0);

        let mut child = command.spawn().with_context(|| {
            format!("failed to launch {} {}", program.display(), args.join(" "))
        })?;
        let pid = child.id();
        let logs: LogBuf = Arc::new(Mutex::new(VecDeque::with_capacity(self.capacity.min(4096))));
        let started_flag = Arc::new(AtomicBool::new(false));
        let active_readers = Arc::new(AtomicUsize::new(0));

        push_line(
            &logs,
            self.capacity,
            format!("$ {} {}", program.display(), args.join(" ")),
        );
        push_line(
            &logs,
            self.capacity,
            format!(
                "  JDK {} ({}) | cwd {}",
                jdk.name,
                jdk.version,
                cwd.display()
            ),
        );

        if let Some(out) = child.stdout.take() {
            active_readers.fetch_add(1, Ordering::AcqRel);
            spawn_reader(
                out,
                logs.clone(),
                started_flag.clone(),
                active_readers.clone(),
                self.capacity,
                false,
            );
        }
        if let Some(err) = child.stderr.take() {
            active_readers.fetch_add(1, Ordering::AcqRel);
            spawn_reader(
                err,
                logs.clone(),
                started_flag.clone(),
                active_readers.clone(),
                self.capacity,
                true,
            );
        }

        let label = format!("{} / {}", project.name, cfg.name);
        self.managed.insert(
            id.clone(),
            Managed {
                id: id.clone(),
                label,
                pid,
                state: RunState::Starting,
                project_path: project.path.clone(),
                started_at: chrono::Local::now().format("%H:%M:%S").to_string(),
                logs,
                started_flag,
                active_readers,
                exited_at: None,
                child,
            },
        );
        Ok(id)
    }

    /// Refresh states from the OS and from the "Started ..." marker in the log stream.
    pub fn poll(&mut self) {
        for m in self.managed.values_mut() {
            if matches!(m.state, RunState::Exited | RunState::Failed) {
                continue;
            }
            if let Ok(Some(status)) = m.child.try_wait() {
                // Allow stdout/stderr reader threads a brief window to flush final
                // error lines before appending "[process exited]" and flipping state.
                let exited_at = *m.exited_at.get_or_insert_with(Instant::now);
                let readers_done = m.active_readers.load(Ordering::Acquire) == 0;
                if !readers_done && exited_at.elapsed() < Duration::from_millis(300) {
                    continue;
                }
                m.state = if status.success() {
                    RunState::Exited
                } else {
                    RunState::Failed
                };
                push_line(
                    &m.logs,
                    self.capacity,
                    format!("[process exited: {status}]"),
                );
                continue;
            }
            if m.started_flag.load(Ordering::Relaxed) {
                m.state = RunState::Running;
            }
        }
    }

    pub fn stop(&mut self, id: &str) -> Result<()> {
        let Some(mut m) = self.managed.remove(id) else {
            anyhow::bail!("no managed process: {id}");
        };
        // Already gone: taskkill rejects a dead pid, and "stop failed" would be
        // a lie for a process that exited on its own.
        if matches!(m.child.try_wait(), Ok(Some(_))) {
            return Ok(());
        }
        push_line(&m.logs, self.capacity, "[stop requested]".into());
        kill_tree(m.pid)?;
        let _ = m.child.wait();
        Ok(())
    }

    /// Stop a process that was started outside this TUI (e.g. from IDEA).
    pub fn stop_external(pid: u32) -> Result<()> {
        kill_tree(pid)
    }

    /// Leave processes running when the TUI detaches. Reader threads keep the
    /// stdout/stderr pipes alive, so dropping `Child` does not break the subprocess.
    pub fn detach_all(&mut self) {
        self.managed.clear();
    }

    pub fn find_by_pid(&self, pid: u32) -> Option<&Managed> {
        self.managed.values().find(|m| m.pid == pid)
    }

    pub fn find_by_project(&self, path: &Path) -> Option<&Managed> {
        self.managed.values().find(|m| m.project_path == path)
    }

    /// Cheap line count for cursor/scroll math (avoids cloning the buffer).
    pub fn log_len(&self, id: &str) -> usize {
        self.managed
            .get(id)
            .map(|m| m.logs.lock().map(|l| l.len()).unwrap_or(0))
            .unwrap_or(0)
    }

    pub fn logs_for(&self, id: &str) -> Vec<String> {
        self.managed
            .get(id)
            .map(|m| {
                m.logs
                    .lock()
                    .map(|l| l.iter().cloned().collect())
                    .unwrap_or_default()
            })
            .unwrap_or_default()
    }
}

pub(crate) fn build_invocation(
    project: &Project,
    cfg: &LaunchConfig,
    maven: Option<&Maven>,
    app_cfg: &AppConfig,
) -> (PathBuf, Vec<String>) {
    let program = match project.build {
        crate::model::BuildTool::Gradle => {
            if project.has_wrapper {
                if cfg!(windows) {
                    project.path.join("gradlew.bat")
                } else {
                    project.path.join("gradlew")
                }
            } else if cfg!(windows) {
                PathBuf::from("gradle.bat")
            } else {
                PathBuf::from("gradle")
            }
        }
        crate::model::BuildTool::Maven => {
            let use_wrapper = project.has_wrapper && cfg.origin == "auto";
            if use_wrapper {
                if cfg!(windows) {
                    project.path.join("mvnw.cmd")
                } else {
                    project.path.join("mvnw")
                }
            } else if let Some(m) = maven {
                m.executable.clone()
            } else if project.has_wrapper {
                if cfg!(windows) {
                    project.path.join("mvnw.cmd")
                } else {
                    project.path.join("mvnw")
                }
            } else if cfg!(windows) {
                PathBuf::from("mvn.cmd")
            } else {
                PathBuf::from("mvn")
            }
        }
    };

    let mut args: Vec<String> = Vec::new();
    match project.build {
        crate::model::BuildTool::Maven => {
            args.push("spring-boot:run".into());
            if cfg.origin != "auto" {
                if let Some(main) = &cfg.main_class {
                    args.push(format!("-Dspring-boot.run.main-class={main}"));
                }
                if !cfg.active_profiles.is_empty() {
                    args.push(format!(
                        "-Dspring-boot.run.profiles={}",
                        cfg.active_profiles
                    ));
                }
                if !cfg.vm_options.is_empty() {
                    args.push(format!("-Dspring-boot.run.jvmArguments={}", cfg.vm_options));
                }
                if !cfg.program_args.is_empty() {
                    args.push(format!("-Dspring-boot.run.arguments={}", cfg.program_args));
                }
            }
        }
        crate::model::BuildTool::Gradle => {
            args.push("bootRun".into());
            if let Some(main) = &cfg.main_class {
                args.push(format!("-PmainClass={main}"));
            }
        }
    }
    args.extend(app_cfg.extra_args.iter().cloned());
    (program, args)
}

/// Windows cannot CreateProcess a .cmd directly; wrap it with cmd.exe /C.
/// Other platforms (including Unix wrapper scripts) execute the program as-is.
fn wrap_command(program: &Path, cwd: &Path) -> Command {
    #[cfg(windows)]
    {
        let ext = program
            .extension()
            .and_then(|s| s.to_str())
            .unwrap_or("")
            .to_lowercase();
        if ext == "cmd" || ext == "bat" {
            let mut c = Command::new("cmd.exe");
            c.arg("/C").arg(program);
            c.current_dir(cwd);
            return c;
        }
    }
    let _ = cwd;
    Command::new(program)
}

fn prepend_path(bin: &Path) -> String {
    let old = std::env::var("PATH").unwrap_or_default();
    let sep = if cfg!(windows) { ";" } else { ":" };
    format!("{}{sep}{old}", bin.display())
}

fn push_line(buf: &LogBuf, cap: usize, line: String) {
    if let Ok(mut guard) = buf.lock() {
        while guard.len() >= cap.max(1) {
            guard.pop_front();
        }
        let stamp = chrono::Local::now().format("%H:%M:%S");
        guard.push_back(format!("[{stamp}] {line}"));
    }
}

struct ReaderGuard(Arc<AtomicUsize>);

impl Drop for ReaderGuard {
    fn drop(&mut self) {
        self.0.fetch_sub(1, Ordering::AcqRel);
    }
}

fn spawn_reader<R: Read + Send + 'static>(
    reader: R,
    buf: LogBuf,
    started: Arc<AtomicBool>,
    active_readers: Arc<AtomicUsize>,
    cap: usize,
    is_err: bool,
) {
    std::thread::spawn(move || {
        let _guard = ReaderGuard(active_readers);
        for line in BufReader::new(reader).split(b'\n') {
            let Ok(bytes) = line else { break };
            // The buffer keeps the escape sequences: the pane turns SGR codes
            // into styles (ansi-to-tui) and flattens them only where plain text
            // is required, e.g. the clipboard or --launch diagnosis.
            let raw = String::from_utf8_lossy(&bytes)
                .trim_end_matches('\r')
                .to_string();
            let plain = plain_text(&raw);
            if plain.trim().is_empty() {
                continue;
            }
            let text = if is_err && !plain.starts_with('[') {
                format!("! {raw}")
            } else {
                raw
            };
            let lower = plain.to_lowercase();
            if lower.contains("started ") && (lower.contains("seconds") || lower.contains(" in "))
                || lower.contains("tomcat started on port")
                || lower.contains("netty started")
            {
                started.store(true, Ordering::Relaxed);
            }
            push_line(&buf, cap, text);
        }
    });
}

/// Kill the whole process tree. `mvn.cmd` forks a java child, so killing only the
/// shell wrapper would leave the Spring Boot app alive.
fn kill_tree(pid: u32) -> Result<()> {
    #[cfg(windows)]
    {
        let status = Command::new("taskkill")
            .args(["/PID", &pid.to_string(), "/T", "/F"])
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status()
            .context("running taskkill")?;
        if !status.success() {
            anyhow::bail!("taskkill failed for pid {pid}");
        }
    }
    #[cfg(unix)]
    {
        let status = Command::new("kill")
            .args(["-TERM", &format!("-{pid}")])
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status()
            .context("sending SIGTERM to process group")?;
        if !status.success() {
            anyhow::bail!("kill failed for process group {pid}");
        }
    }
    Ok(())
}

/// Flatten one log line to the text a terminal would have shown: SGR codes
/// carry styling for the pane, every other escape sequence is dropped.
/// Used wherever the raw buffer feeds a plain-text sink (clipboard, --launch
/// diagnosis); rendering goes through `ansi-to-tui` instead.
pub(crate) fn plain_text(line: &str) -> String {
    // Fast path: Maven and plain println output never carry escape sequences.
    if !line.contains('\u{1b}') {
        return line.to_owned();
    }
    use ansi_to_tui::IntoText as _;
    match line.as_bytes().into_text() {
        Ok(text) => text
            .lines
            .iter()
            .flat_map(|l| l.spans.iter())
            .map(|s| s.content.as_ref())
            .collect(),
        // Malformed input: better to show the raw line than to lose it.
        Err(_) => line.to_owned(),
    }
}
