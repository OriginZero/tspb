use crate::config::Config;
use crate::event::{Action, parse_prefix};
use crate::model::*;
use crate::runner::Runner;
use crate::scan;
use anyhow::Result;
use crossterm::event::{MouseButton, MouseEvent, MouseEventKind};
use ratatui::layout::Rect;
use std::cell::RefCell;
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant, SystemTime};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Focus {
    Sidebar,
    List,
    Log,
}

impl Focus {
    pub fn label(self) -> &'static str {
        match self {
            Self::Sidebar => "WORKSPACES",
            Self::List => "ITEMS",
            Self::Log => "LOG",
        }
    }
}

/// Vim-like two-layer input model.
///
/// * `Panes` (default): **no window is active**. h/j/k/l picks a window,
///   Enter enters it. No pane border or list row is highlighted as focused.
/// * `Content`: one window is active. h/j/k/l navigates *inside* that window,
///   Enter acts on the current item, Esc returns to `Panes`.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub enum Layer {
    #[default]
    Panes,
    Content,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Mode {
    Normal,
    Filter,
    Help,
    Goto,
    JdkPicker,
    MavenPicker,
    ConfigPicker,
    ConfirmStop,
    ConfirmQuit,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum StatusFilter {
    Working,
    Blocked,
    Idle,
    Done,
}

impl StatusFilter {
    pub fn from_key(c: char) -> Option<Self> {
        match c {
            'w' => Some(Self::Working),
            'b' => Some(Self::Blocked),
            'i' => Some(Self::Idle),
            'd' => Some(Self::Done),
            _ => None,
        }
    }
    pub fn matches(self, state: RunState) -> bool {
        match self {
            Self::Working => state == RunState::Starting,
            Self::Blocked => state == RunState::Failed,
            Self::Idle => matches!(state, RunState::Idle | RunState::Exited),
            Self::Done => state == RunState::Running,
        }
    }
    pub fn label(self) -> &'static str {
        match self {
            Self::Working => "WORKING",
            Self::Blocked => "BLOCKED",
            Self::Idle => "IDLE",
            Self::Done => "DONE",
        }
    }
}

#[derive(Clone, Copy, Debug, Default)]
pub struct Hitboxes {
    pub sidebar: Rect,
    pub list: Rect,
    pub detail: Rect,
    pub log: Rect,
}

pub struct LineEntry {
    pub row: Option<usize>,
    pub group: String,
}

#[derive(Clone, Debug)]
pub struct FileLogState {
    pub pid: Option<u32>,
    pub path: PathBuf,
    pub last_read: Instant,
    pub last_len: u64,
    pub last_modified: Option<SystemTime>,
    pub lines: Vec<String>,
}

pub type LogFileCache = RefCell<HashMap<(Option<usize>, Option<u32>), (Instant, Option<PathBuf>)>>;

pub struct App {
    pub cfg: Config,
    pub inv: Inventory,
    pub runner: Runner,
    pub view: View,
    pub focus: Focus,
    pub layer: Layer,
    /// The window highlighted while in the PANES layer. `None` on startup:
    /// literally nothing is selected until the first h/j/k/l.
    pub hover: Option<Focus>,
    pub mode: Mode,
    pub prefix_active: bool,
    pub prefix_char: char,
    pub show_sidebar: bool,
    pub show_log: bool,
    pub zoom: Option<Focus>,
    pub rows: Vec<Row>,
    pub selected: usize,
    pub sidebar_selected: usize,
    pub picker_selected: usize,
    pub filter: String,
    pub goto_filter: String,
    pub help_scroll: usize,
    pub status_filter: Option<StatusFilter>,
    pub active_jdk: usize,
    pub jdk_pinned: bool,
    pub active_maven: usize,
    pub active_config: HashMap<String, usize>,
    pub selected_log_id: Option<String>,
    pub file_logs: HashMap<String, FileLogState>,
    log_file_cache: LogFileCache,
    pub log_follow: bool,
    pub log_scroll: usize,
    /// Absolute index of the yank/scroll cursor inside the log ring buffer.
    pub log_cursor: usize,
    /// The stream the LOG pane showed last tick; a change resets scroll/follow.
    pub shown_log_id: Option<String>,
    /// `v` anchors a visual line selection; `y` yanks anchor..cursor.
    pub visual_anchor: Option<usize>,
    /// Last yanked text (also the fallback when no OS clipboard is reachable).
    pub clipboard: Option<String>,
    /// Number of visible log lines, written by the renderer each frame.
    pub log_view_height: usize,
    /// Current scroll offset of the ITEMS list widget, written by the renderer.
    pub list_scroll_offset: usize,
    pub message: String,
    pub scan_root: std::path::PathBuf,
    pub confirm_pid: Option<u32>,
    pub quit_stop: bool,
    pub should_quit: bool,
    pub hitboxes: Hitboxes,
    pub last_proc_refresh: Instant,
}

impl App {
    /// Wrap an inventory that was collected elsewhere (e.g. by the startup
    /// progress worker) in UI state, including the first status line.
    pub fn from_inventory(cfg: Config, inv: Inventory) -> Self {
        let mut app = Self::with_inventory(cfg, inv);
        let dir = app
            .scan_root
            .file_name()
            .map(|s| s.to_string_lossy().into_owned())
            .unwrap_or_else(|| app.scan_root.to_string_lossy().into_owned());
        app.message = if app.inv.projects.is_empty() {
            format!("no Spring Boot app in {dir} - try --root DIR")
        } else {
            format!(
                "{} items · PANES: h/j/k/l pick window, Enter enter, ? help",
                app.inv.projects.len()
            )
        };
        app
    }

    /// Build UI state from a pre-built inventory. Keeps the state machine and
    /// renderer testable without a real filesystem scan.
    pub fn with_inventory(cfg: Config, inv: Inventory) -> Self {
        let mut app = Self {
            runner: Runner::new(cfg.log_capacity),
            inv,
            view: View::All,
            focus: Focus::List,
            layer: Layer::Panes,
            hover: None,
            mode: Mode::Normal,
            prefix_active: false,
            prefix_char: parse_prefix(&cfg.prefix),
            show_sidebar: true,
            show_log: true,
            zoom: None,
            rows: Vec::new(),
            selected: 0,
            sidebar_selected: 0,
            picker_selected: 0,
            filter: String::new(),
            goto_filter: String::new(),
            help_scroll: 0,
            status_filter: None,
            active_jdk: 0,
            jdk_pinned: cfg.preferred_jdk.is_some(),
            active_maven: 0,
            active_config: HashMap::new(),
            selected_log_id: None,
            file_logs: HashMap::new(),
            log_file_cache: RefCell::new(HashMap::new()),
            log_follow: true,
            log_scroll: 0,
            log_cursor: 0,
            shown_log_id: None,
            visual_anchor: None,
            clipboard: None,
            log_view_height: 10,
            list_scroll_offset: 0,
            message: String::new(),
            scan_root: cfg.effective_root(),
            confirm_pid: None,
            quit_stop: false,
            should_quit: false,
            hitboxes: Hitboxes::default(),
            last_proc_refresh: Instant::now(),
            cfg,
        };
        app.pick_default_tools();
        app.rebuild_rows();
        // Land on the first launchable item: the primary action is Enter -> start.
        if let Some(i) = app.rows.iter().position(|r| r.group == "PROJECTS") {
            app.selected = i;
        }
        app
    }

    pub fn is_editing(&self) -> bool {
        matches!(self.mode, Mode::Filter | Mode::Goto)
    }

    fn pick_default_tools(&mut self) {
        if let Some(pref) = self.cfg.preferred_jdk.as_ref() {
            if let Some(i) = self
                .inv
                .jdks
                .iter()
                .position(|j| j.name.to_lowercase().contains(&pref.to_lowercase()))
            {
                self.active_jdk = i;
            }
        } else {
            // Prefer a JDK matching the Java version declared by the scanned poms;
            // an IDE-bundled JBR 25 must not silently run a Java 17 project.
            let want = self
                .inv
                .projects
                .iter()
                .filter_map(|p| p.java_version)
                .max();
            self.active_jdk = want
                .and_then(|w| {
                    self.inv
                        .jdks
                        .iter()
                        .position(|j| jdk_major(&j.version) == Some(w))
                })
                .or_else(|| {
                    self.inv
                        .jdks
                        .iter()
                        .position(|j| jdk_major(&j.version).is_some_and(|v| v >= 17))
                })
                .unwrap_or(0);
        }
        if let Some(pref) = self.cfg.preferred_maven.as_ref() {
            if let Some(i) = self
                .inv
                .mavens
                .iter()
                .position(|m| m.name.to_lowercase().contains(&pref.to_lowercase()))
            {
                self.active_maven = i;
            }
        } else if let Some(i) = self
            .inv
            .mavens
            .iter()
            .position(|m| m.origin == Origin::Idea)
        {
            self.active_maven = i;
        }
    }

    // ---------------------------------------------------------------- rows

    fn match_running_project(&self, app: &RunningApp) -> Option<usize> {
        if let Some(wd) = app.work_dir.as_deref()
            && let Some(pi) = self.inv.projects.iter().position(|p| p.path == wd)
        {
            return Some(pi);
        }
        let main_class = app.main_class.as_str();
        let short = main_class.rsplit('.').next().unwrap_or(main_class);
        let cmd_norm = app.cmdline.replace('\\', "/").to_lowercase();
        let matches_main = |p: &Project| {
            p.main_class.as_deref() == Some(main_class)
                || p.launch_configs.iter().any(|c| {
                    c.main_class
                        .as_deref()
                        .is_some_and(|m| m == main_class || m.rsplit('.').next() == Some(short))
                })
        };

        let candidates: Vec<usize> = self
            .inv
            .projects
            .iter()
            .enumerate()
            .filter(|(_, p)| matches_main(p))
            .map(|(i, _)| i)
            .collect();

        if candidates.len() > 1 {
            // Disambiguate modules sharing a main class by path in cmdline or IDEA <module>.
            if let Some(&pi) = candidates.iter().find(|&&i| {
                let p_norm = self.inv.projects[i]
                    .path
                    .to_string_lossy()
                    .replace('\\', "/")
                    .to_lowercase();
                !p_norm.is_empty() && cmd_norm.contains(&p_norm)
            }) {
                return Some(pi);
            }
            if let Some(&pi) = candidates.iter().find(|&&i| {
                let p = &self.inv.projects[i];
                p.launch_configs.iter().any(|c| {
                    c.main_class.as_deref() == Some(main_class)
                        && c.idea_module
                            .as_deref()
                            .is_some_and(|m| m.eq_ignore_ascii_case(&p.name))
                })
            }) {
                return Some(pi);
            }
        }
        if let Some(&first) = candidates.first() {
            return Some(first);
        }
        self.inv
            .projects
            .iter()
            .position(|p| p.name.eq_ignore_ascii_case(&app.name))
    }

    fn is_deduped_running_app(&self, a: &RunningApp) -> bool {
        if self.runner.find_by_pid(a.pid).is_some() {
            return true;
        }
        self.runner.managed.values().any(|m| {
            matches!(m.state, RunState::Starting | RunState::Running)
                && (self
                    .match_running_project(a)
                    .is_some_and(|pi| self.inv.projects[pi].path == m.project_path)
                    || a.work_dir.as_deref() == Some(&m.project_path))
        })
    }

    pub fn rebuild_rows(&mut self) {
        let old_key = self.rows.get(self.selected).map(|r| self.row_key(r));
        let mut rows: Vec<Row> = Vec::new();
        let view = self.view;
        let show_running = matches!(view, View::All | View::Running);
        let show_projects = matches!(view, View::All | View::Projects);
        let tool_filter: Option<bool> = match view {
            View::EnvTools => Some(false),
            View::IdeaTools => Some(true),
            _ => None,
        };
        let show_tools = self.status_filter.is_none()
            && matches!(view, View::All | View::EnvTools | View::IdeaTools);

        // Managed processes started by this TUI first (they are the freshest state).
        if show_running {
            let mut managed_list: Vec<_> = self.runner.managed.values().collect();
            managed_list.sort_by(|a, b| a.label.cmp(&b.label).then_with(|| a.pid.cmp(&b.pid)));
            for m in managed_list {
                let ports = self
                    .inv
                    .running
                    .iter()
                    .find_map(|a| {
                        let is_match = a.pid == m.pid
                            || (self
                                .match_running_project(a)
                                .is_some_and(|pi| self.inv.projects[pi].path == m.project_path))
                            || a.work_dir.as_deref() == Some(&m.project_path);
                        if is_match && !a.ports.is_empty() {
                            Some(format!(
                                " · :{}",
                                a.ports
                                    .iter()
                                    .map(u16::to_string)
                                    .collect::<Vec<_>>()
                                    .join(",")
                            ))
                        } else {
                            None
                        }
                    })
                    .unwrap_or_default();

                rows.push(Row {
                    kind: RowKind::App {
                        project: self
                            .inv
                            .projects
                            .iter()
                            .position(|p| p.path == m.project_path),
                        pid: Some(m.pid),
                    },
                    group: "RUNNING".into(),
                    label: m.label.clone(),
                    origin: Origin::Idea,
                    state: m.state,
                    detail: format!(
                        "pid {} · started {} · managed{}",
                        m.pid, m.started_at, ports
                    ),
                });
            }
            for a in &self.inv.running {
                if self.is_deduped_running_app(a) {
                    continue;
                }
                let ports = if a.ports.is_empty() {
                    String::new()
                } else {
                    format!(
                        " · :{}",
                        a.ports
                            .iter()
                            .map(u16::to_string)
                            .collect::<Vec<_>>()
                            .join(",")
                    )
                };
                rows.push(Row {
                    kind: RowKind::App {
                        project: self.match_running_project(a),
                        pid: Some(a.pid),
                    },
                    group: "RUNNING".into(),
                    label: a.name.clone(),
                    origin: Origin::System,
                    state: RunState::Running,
                    detail: format!("pid {} · {}{}", a.pid, a.main_class, ports),
                });
            }
        }

        if show_projects {
            for (i, p) in self.inv.projects.iter().enumerate() {
                let managed = self.runner.find_by_project(&p.path);
                let state = managed.map(|m| m.state).unwrap_or(RunState::Idle);
                let cfg_name = self
                    .config_of(i)
                    .map(|c| c.name.clone())
                    .unwrap_or_else(|| "spring-boot:run".into());
                rows.push(Row {
                    kind: RowKind::App {
                        project: Some(i),
                        pid: managed.map(|m| m.pid),
                    },
                    group: "PROJECTS".into(),
                    label: p.name.clone(),
                    origin: p.origin,
                    state,
                    detail: format!("{} · {} · {}", p.build.label(), cfg_name, p.path.display()),
                });
            }
        }

        if show_tools {
            for (i, j) in self.inv.jdks.iter().enumerate() {
                if tool_filter.is_some_and(|want_idea| (j.origin == Origin::Idea) != want_idea) {
                    continue;
                }
                rows.push(Row {
                    kind: RowKind::Jdk(i),
                    group: "JDK".into(),
                    label: format!("{} {}", j.name, j.version),
                    origin: j.origin,
                    state: RunState::Idle,
                    detail: j.home.display().to_string(),
                });
            }
            for (i, m) in self.inv.mavens.iter().enumerate() {
                if tool_filter.is_some_and(|want_idea| (m.origin == Origin::Idea) != want_idea) {
                    continue;
                }
                rows.push(Row {
                    kind: RowKind::Maven(i),
                    group: "MAVEN".into(),
                    label: format!("{} {}", m.name, m.version),
                    origin: m.origin,
                    state: RunState::Idle,
                    detail: m.home.display().to_string(),
                });
            }
        }

        let needle = self.filter.trim().to_lowercase();
        rows.retain(|r| {
            let status_ok = match r.kind {
                RowKind::App { .. } => self
                    .status_filter
                    .map(|f| f.matches(r.state))
                    .unwrap_or(true),
                _ => self.status_filter.is_none(),
            };
            status_ok && (needle.is_empty() || r.search_text().to_lowercase().contains(&needle))
        });
        self.rows = rows;
        self.selected = old_key
            .and_then(|k| self.rows.iter().position(|r| self.row_key(r) == k))
            .unwrap_or(self.selected)
            .min(self.rows.len().saturating_sub(1));
    }

    pub fn lines(&self) -> Vec<LineEntry> {
        let mut out = Vec::with_capacity(self.rows.len() + 4);
        let mut last = String::new();
        for (i, r) in self.rows.iter().enumerate() {
            if r.group != last {
                out.push(LineEntry {
                    row: None,
                    group: r.group.clone(),
                });
                last = r.group.clone();
            }
            out.push(LineEntry {
                row: Some(i),
                group: r.group.clone(),
            });
        }
        out
    }

    pub fn selected_line(&self) -> Option<usize> {
        self.lines()
            .iter()
            .position(|l| l.row == Some(self.selected))
    }

    pub fn current_row(&self) -> Option<&Row> {
        self.rows.get(self.selected)
    }

    fn running_row_count(&self) -> usize {
        self.runner.managed.len()
            + self
                .inv
                .running
                .iter()
                .filter(|a| !self.is_deduped_running_app(a))
                .count()
    }

    pub fn view_count(&self, view: View) -> usize {
        match view {
            View::All => {
                self.running_row_count()
                    + self.inv.projects.len()
                    + self.inv.jdks.len()
                    + self.inv.mavens.len()
            }
            View::Running => self.running_row_count(),
            View::Projects => self.inv.projects.len(),
            View::EnvTools => {
                self.inv
                    .jdks
                    .iter()
                    .filter(|j| j.origin != Origin::Idea)
                    .count()
                    + self
                        .inv
                        .mavens
                        .iter()
                        .filter(|m| m.origin != Origin::Idea)
                        .count()
            }
            View::IdeaTools => {
                self.inv
                    .jdks
                    .iter()
                    .filter(|j| j.origin == Origin::Idea)
                    .count()
                    + self
                        .inv
                        .mavens
                        .iter()
                        .filter(|m| m.origin == Origin::Idea)
                        .count()
            }
        }
    }

    // ---------------------------------------------------------------- toolchain helpers

    pub fn jdk_label(&self) -> String {
        self.inv
            .jdks
            .get(self.active_jdk)
            .map(|j| format!("{} ({})", j.name, j.version))
            .unwrap_or_else(|| "none".into())
    }

    pub fn maven_label(&self) -> String {
        self.inv
            .mavens
            .get(self.active_maven)
            .map(|m| format!("{} ({})", m.name, m.version))
            .unwrap_or_else(|| "PATH mvn".into())
    }

    fn config_key(&self, pi: usize) -> String {
        self.inv.projects[pi].path.display().to_string()
    }

    fn config_index(&self, pi: usize) -> usize {
        let key = self.config_key(pi);
        let len = self.inv.projects[pi].launch_configs.len().max(1);
        self.active_config
            .get(&key)
            .copied()
            .unwrap_or(0)
            .min(len - 1)
    }

    pub fn config_of(&self, pi: usize) -> Option<&LaunchConfig> {
        self.inv
            .projects
            .get(pi)?
            .launch_configs
            .get(self.config_index(pi))
    }

    pub fn current_config_label(&self) -> String {
        self.current_row()
            .and_then(|r| match r.kind {
                RowKind::App {
                    project: Some(pi), ..
                } => Some(pi),
                _ => None,
            })
            .and_then(|pi| self.config_of(pi).map(|c| c.name.clone()))
            .unwrap_or_else(|| "-".into())
    }

    pub fn find_log_file_for_row(&self, row: &Row) -> Option<PathBuf> {
        let (project, pid) = match row.kind {
            RowKind::App { project, pid } => (project, pid),
            _ => return None,
        };
        let key = (project, pid);
        if let Some((checked_at, cached)) = self.log_file_cache.borrow().get(&key)
            && checked_at.elapsed() < Duration::from_secs(2)
        {
            return cached.clone();
        }
        let running_app = pid.and_then(|p| self.inv.running.iter().find(|a| a.pid == p));
        let project_obj = project.and_then(|pi| self.inv.projects.get(pi));

        let project_path = project_obj.map(|p| p.path.as_path());
        let project_name = project_obj.map(|p| p.name.as_str());
        let main_class = running_app
            .map(|a| a.main_class.as_str())
            .or_else(|| project_obj.and_then(|p| p.main_class.as_deref()));
        let app_name = running_app
            .map(|a| a.name.as_str())
            .or_else(|| project_obj.map(|p| p.name.as_str()));
        let work_dir = running_app.and_then(|a| a.work_dir.as_deref());
        let cmdline = running_app.map(|a| a.cmdline.as_str());

        let found = crate::scan::project::find_log_file(
            project_path,
            project_name,
            main_class,
            app_name,
            work_dir,
            cmdline,
            &self.cfg.scan_roots,
        );
        self.log_file_cache
            .borrow_mut()
            .insert(key, (Instant::now(), found.clone()));
        found
    }

    /// The stream the LOG pane renders. The selected row wins, so moving the
    /// cursor over a RUNNING entry switches the pane immediately; the pid match
    /// keeps two launch configs of the same module apart. `selected_log_id` is
    /// only a fallback for rows that carry no stream (JDK / Maven / idle project),
    /// which keeps the pane from blanking out while you browse toolchains.
    pub fn current_log_id(&self) -> Option<String> {
        let from_row = self.current_row().and_then(|row| match row.kind {
            RowKind::App { project, pid } => {
                if let Some(pid) = pid
                    && let Some(m) = self.runner.find_by_pid(pid)
                {
                    return Some(m.id.clone());
                }
                if let Some(pi) = project
                    && let Some(m) = self.runner.find_by_project(&self.inv.projects[pi].path)
                {
                    return Some(m.id.clone());
                }
                if pid.is_some() {
                    return self
                        .find_log_file_for_row(row)
                        .map(|p| format!("file://{}", p.display()));
                }
                None
            }
            _ => None,
        });
        from_row.or_else(|| {
            self.selected_log_id.clone().filter(|id| {
                self.runner.managed.contains_key(id)
                    || (id.starts_with("file://") && self.is_live_file_log(id))
            })
        })
    }

    fn is_live_file_log(&self, id: &str) -> bool {
        if self.inv.running.is_empty() {
            return false;
        }
        match self.file_logs.get(id).and_then(|s| s.pid) {
            Some(pid) => self.inv.running.iter().any(|a| a.pid == pid),
            None => false,
        }
    }

    pub fn logs_for(&self, id: &str) -> Vec<String> {
        if let Some(path_str) = id.strip_prefix("file://") {
            if let Some(state) = self.file_logs.get(id) {
                return state.lines.clone();
            }
            let path = Path::new(path_str);
            if let Ok(lines) = crate::scan::project::read_file_tail(path, 2000) {
                return lines;
            }
            return Vec::new();
        }
        self.runner.logs_for(id)
    }

    pub fn log_len_of(&self, id: &str) -> usize {
        if let Some(path_str) = id.strip_prefix("file://") {
            if let Some(state) = self.file_logs.get(id) {
                return state.lines.len();
            }
            let path = Path::new(path_str);
            if let Ok(lines) = crate::scan::project::read_file_tail(path, 2000) {
                return lines.len();
            }
            return 0;
        }
        self.runner.log_len(id)
    }

    // ---------------------------------------------------------------- actions

    pub fn handle(&mut self, action: Action) {
        // Key-up / auto-repeat pseudo-events are not user input: dropping them
        // here keeps one keystroke = one action and leaves a pending prefix intact.
        if action == Action::Ignored {
            return;
        }
        if action != Action::Prefix {
            self.prefix_active = false;
        }
        // Overlays keep classic relative navigation: inside a picker / help /
        // goto panel a Vim key simply means "move one line in that overlay".
        let action = if self.mode != Mode::Normal {
            match action {
                Action::Vim('j') | Action::Vim('l') => Action::Move(1),
                Action::Vim('k') | Action::Vim('h') => Action::Move(-1),
                other => other,
            }
        } else {
            action
        };
        match self.mode {
            Mode::Normal => self.handle_normal(action),
            Mode::Filter => self.handle_filter(action),
            Mode::Goto => self.handle_goto(action),
            Mode::Help => self.handle_help(action),
            Mode::JdkPicker | Mode::MavenPicker | Mode::ConfigPicker => self.handle_picker(action),
            Mode::ConfirmStop | Mode::ConfirmQuit => self.handle_confirm(action),
        }
    }

    fn handle_normal(&mut self, action: Action) {
        // ---- layer-aware navigation: the same key means different things in
        // PANES (pick a window) and CONTENT (move inside that window). --------
        match action {
            // One key, no prefix: full screen the window you are in (CONTENT) or
            // the one you highlighted (PANES). Nothing targeted -> nothing to do.
            Action::Vim('z') if self.hover.is_some() => return self.toggle_zoom(),
            Action::Vim(c) => return self.navigate(c),
            Action::Move(d) => return self.navigate_move(d),
            Action::Enter => {
                if self.layer == Layer::Panes {
                    self.enter_pane(self.hover.unwrap_or(Focus::List));
                } else {
                    self.primary_action();
                }
                return;
            }
            Action::Esc => return self.escape(),
            Action::Focus(n) => {
                let f = match n {
                    0 => Focus::Sidebar,
                    2 => Focus::Log,
                    _ => Focus::List,
                };
                return self.enter_pane(f);
            }
            Action::CopyMode => {
                self.enter_pane(Focus::Log);
                self.message =
                    "LOG: j/k scroll · v select · y yank · Y all · G tail · Esc panes".into();
                return;
            }
            Action::Visual if self.is_log_content() => return self.toggle_visual(),
            Action::CopyLine | Action::CopyAll if self.is_log_content() => {
                return self.copy_log(action == Action::CopyAll);
            }
            Action::Goto | Action::Home if self.is_log_content() => {
                return self.log_jump(0, false);
            }
            Action::End if self.is_log_content() => {
                let last = self.log_len().saturating_sub(1);
                return self.log_jump(last, true);
            }
            Action::PageDown | Action::PageUp if self.is_log_content() => {
                return self.log_page(if action == Action::PageDown { 1 } else { -1 });
            }
            Action::PageDown | Action::PageUp => {
                return self.navigate_move(if action == Action::PageDown { 10 } else { -10 });
            }
            Action::Home => {
                if self.layer == Layer::Content && self.focus == Focus::Sidebar {
                    self.set_view(View::ALL[0]);
                } else {
                    self.selected = 0;
                }
                return;
            }
            Action::End => {
                if self.layer == Layer::Content && self.focus == Focus::Sidebar {
                    if let Some(&last) = View::ALL.last() {
                        self.set_view(last);
                    }
                } else {
                    self.selected = self.rows.len().saturating_sub(1);
                }
                return;
            }
            _ => {}
        }

        // ---- global / item actions (available in either layer) --------------
        match action {
            // `handle` already returns early for Ignored; this arm only keeps
            // the exhaustive match honest.
            Action::None | Action::Ignored => {}
            Action::Prefix => self.prefix_active = true,
            Action::Quit(stop) => {
                if stop && !self.runner.managed.is_empty() {
                    self.quit_stop = true;
                    self.mode = Mode::ConfirmQuit;
                } else {
                    self.should_quit = true;
                }
            }
            Action::Help => {
                self.mode = Mode::Help;
                self.help_scroll = 0;
            }
            Action::Goto => {
                self.mode = Mode::Goto;
                self.goto_filter.clear();
                self.picker_selected = self.selected;
            }
            Action::FilterMode => {
                self.mode = Mode::Filter;
                self.filter.clear();
            }
            Action::Move(d) => self.move_selection(d),
            Action::PageDown => self.move_selection(10),
            Action::PageUp => self.move_selection(-10),
            Action::Home => {
                self.selected = 0;
            }
            Action::End => {
                self.selected = self.rows.len().saturating_sub(1);
            }
            Action::Enter => self.primary_action(),
            Action::Esc => {
                self.filter.clear();
                self.status_filter = None;
                self.message.clear();
                self.zoom = None;
                self.rebuild_rows();
            }
            Action::FilterBackspace => {
                if !self.filter.is_empty() {
                    self.filter.pop();
                    self.rebuild_rows();
                }
            }
            Action::StatusFilter(c) => {
                if c == 'a' {
                    self.status_filter = None;
                } else {
                    let f = StatusFilter::from_key(c).unwrap();
                    self.status_filter = if self.status_filter == Some(f) {
                        None
                    } else {
                        Some(f)
                    };
                }
                self.rebuild_rows();
            }
            Action::Stop => self.stop_selected(),
            Action::Restart => self.restart_selected(),
            Action::Refresh => self.refresh_all(),
            Action::Focus(n) => self.set_focus(n),
            Action::ToggleSidebar => {
                self.show_sidebar = !self.show_sidebar;
                if !self.show_sidebar {
                    if self.focus == Focus::Sidebar {
                        self.reset_hidden_focus(Focus::List);
                    } else if self.zoom == Some(Focus::Sidebar) {
                        self.zoom = None;
                    }
                }
            }
            Action::ToggleLog => self.toggle_log_pane(),
            Action::CloseLog => {
                if self.show_log {
                    self.toggle_log_pane();
                }
            }
            Action::Zoom => self.toggle_zoom(),
            Action::NextView => self.shift_view(1),
            Action::PrevView => self.shift_view(-1),
            Action::View(i) => {
                if let Some(v) = View::ALL.get(i) {
                    self.set_view(*v);
                }
            }
            Action::JdkPicker => {
                if self.inv.jdks.is_empty() {
                    self.message = "no JDK found - press r to rescan".into();
                } else {
                    self.mode = Mode::JdkPicker;
                    self.picker_selected = self.active_jdk;
                }
            }
            Action::MavenPicker => {
                if self.inv.mavens.is_empty() {
                    self.message = "no Maven found - press r to rescan".into();
                } else {
                    self.mode = Mode::MavenPicker;
                    self.picker_selected = self.active_maven;
                }
            }
            Action::ConfigPicker => {
                if self.current_config_count() > 1 {
                    self.mode = Mode::ConfigPicker;
                    self.picker_selected = self
                        .current_row()
                        .and_then(|r| match r.kind {
                            RowKind::App {
                                project: Some(pi), ..
                            } => Some(self.config_index(pi)),
                            _ => None,
                        })
                        .unwrap_or(0);
                } else {
                    self.message = "only one launch configuration for this project".into();
                }
            }
            Action::NextConfig => {
                let n = self.current_config_count();
                if n > 1
                    && let Some(pi) = self.current_row().and_then(|r| match r.kind {
                        RowKind::App {
                            project: Some(i), ..
                        } => Some(i),
                        _ => None,
                    })
                {
                    let next = (self.config_index(pi) + 1) % n;
                    self.active_config.insert(self.config_key(pi), next);
                    self.rebuild_rows();
                    self.message = format!(
                        "launch config: {}",
                        self.config_of(pi)
                            .map(|c| c.name.clone())
                            .unwrap_or_default()
                    );
                }
            }
            Action::CopyMode => {
                self.show_log = true;
                self.focus = Focus::Log;
                self.log_follow = false;
                self.message = "log scroll mode - PgUp/PgDn, End to follow".into();
            }
            Action::Mouse(m) => self.handle_mouse(m),
            Action::FilterChar(_) => {}
            // The actions below were already handled by the layer-aware match
            // above when relevant; these arms keep the match exhaustive and give
            // `v` its old "toggle the log pane" meaning outside the LOG window.
            Action::Vim(_) => {}
            Action::Visual => self.toggle_log_pane(),
            Action::CopyLine | Action::CopyAll => {
                self.message = "yank needs the LOG window - pick LOG with j, then Enter".into();
            }
        }
    }

    fn handle_filter(&mut self, action: Action) {
        match action {
            Action::FilterChar(c) => {
                self.filter.push(c);
                self.rebuild_rows();
            }
            Action::FilterBackspace => {
                self.filter.pop();
                self.rebuild_rows();
            }
            Action::Enter | Action::Esc => {
                if action == Action::Esc {
                    self.filter.clear();
                    self.rebuild_rows();
                }
                self.mode = Mode::Normal;
            }
            Action::Move(_) => {}
            _ => {}
        }
    }

    fn handle_goto(&mut self, action: Action) {
        match action {
            Action::FilterChar(c) => {
                self.goto_filter.push(c);
                self.picker_selected = 0;
            }
            Action::FilterBackspace => {
                self.goto_filter.pop();
            }
            Action::Move(d) => {
                let n = self.goto_items().len();
                self.picker_selected = clamp_move(self.picker_selected, d, n);
            }
            Action::Enter => {
                let items = self.goto_items();
                if let Some((_, row_idx)) = items.get(self.picker_selected) {
                    self.selected = *row_idx;
                    self.enter_pane(Focus::List);
                }
                self.mode = Mode::Normal;
            }
            Action::Esc => self.mode = Mode::Normal,
            _ => {}
        }
    }

    pub fn goto_items(&self) -> Vec<(String, usize)> {
        let needle = self.goto_filter.trim().to_lowercase();
        let mut counts: HashMap<(&str, &str), usize> = HashMap::new();
        for r in &self.rows {
            *counts
                .entry((r.group.as_str(), r.label.as_str()))
                .or_insert(0) += 1;
        }
        self.rows
            .iter()
            .enumerate()
            .filter(|(_, r)| needle.is_empty() || r.search_text().to_lowercase().contains(&needle))
            .map(|(i, r)| {
                let dup = counts
                    .get(&(r.group.as_str(), r.label.as_str()))
                    .copied()
                    .unwrap_or(0)
                    > 1;
                let title = if dup {
                    if let RowKind::App {
                        project: Some(pi), ..
                    } = r.kind
                    {
                        format!(
                            "[{}] {} ({})",
                            r.group,
                            r.label,
                            self.inv.projects[pi].path.display()
                        )
                    } else {
                        format!("[{}] {} ({})", r.group, r.label, r.detail)
                    }
                } else {
                    format!("[{}] {}", r.group, r.label)
                };
                (title, i)
            })
            .collect()
    }

    fn handle_help(&mut self, action: Action) {
        match action {
            Action::Move(d) => self.help_scroll = clamp_move(self.help_scroll, d, 40),
            Action::PageDown => self.help_scroll = clamp_move(self.help_scroll, 10, 40),
            Action::PageUp => self.help_scroll = clamp_move(self.help_scroll, -10, 40),
            Action::Home => self.help_scroll = 0,
            Action::Enter | Action::Esc | Action::Help => self.mode = Mode::Normal,
            Action::Quit(_) => self.should_quit = true,
            Action::FilterMode => self.mode = Mode::Normal,
            _ => {}
        }
    }

    fn handle_picker(&mut self, action: Action) {
        let n = match self.mode {
            Mode::JdkPicker => self.inv.jdks.len(),
            Mode::MavenPicker => self.inv.mavens.len(),
            Mode::ConfigPicker => self.current_config_count(),
            _ => 0,
        };
        match action {
            Action::Move(d) => self.picker_selected = clamp_move(self.picker_selected, d, n),
            Action::Enter => {
                match self.mode {
                    Mode::JdkPicker => {
                        self.active_jdk = self.picker_selected;
                        self.jdk_pinned = true;
                        self.message = format!("JDK -> {}", self.jdk_label());
                    }
                    Mode::MavenPicker => {
                        self.active_maven = self.picker_selected;
                        self.message = format!("Maven -> {}", self.maven_label());
                    }
                    Mode::ConfigPicker => {
                        if let Some(pi) = self.current_row().and_then(|r| match r.kind {
                            RowKind::App {
                                project: Some(i), ..
                            } => Some(i),
                            _ => None,
                        }) {
                            self.active_config
                                .insert(self.config_key(pi), self.picker_selected);
                            self.message =
                                format!("launch config -> {}", self.current_config_label());
                            self.rebuild_rows();
                        }
                    }
                    _ => {}
                }
                self.mode = Mode::Normal;
            }
            Action::Esc => self.mode = Mode::Normal,
            _ => {}
        }
    }

    fn handle_confirm(&mut self, action: Action) {
        let confirm = matches!(action, Action::Enter);
        let cancel = matches!(action, Action::Esc);
        if !confirm && !cancel {
            return;
        }
        let was_stop = self.mode == Mode::ConfirmStop;
        self.mode = Mode::Normal;
        if !confirm {
            self.confirm_pid = None;
            return;
        }
        if was_stop {
            if let Some(pid) = self.confirm_pid.take() {
                if let Some(m) = self.runner.find_by_pid(pid) {
                    let id = m.id.clone();
                    self.finish_stop_managed(&id, false);
                } else {
                    match Runner::stop_external(pid) {
                        Ok(()) => {
                            self.reset_log_stream();
                            self.file_logs.retain(|_, s| s.pid != Some(pid));
                            self.message = format!("sent stop to pid {pid}");
                            self.refresh_processes();
                        }
                        Err(e) => self.message = format!("stop failed: {e}"),
                    }
                }
            }
        } else {
            for id in self.runner.managed.keys().cloned().collect::<Vec<_>>() {
                let _ = self.stop_managed(&id);
            }
            self.should_quit = true;
        }
    }

    // ---------------------------------------------------------------- vim layers

    pub fn is_log_content(&self) -> bool {
        self.layer == Layer::Content && self.focus == Focus::Log
    }

    fn navigate(&mut self, c: char) {
        match self.layer {
            Layer::Panes => self.navigate_panes(c),
            Layer::Content => self.navigate_content(c),
        }
    }

    /// In PANES nothing is active yet: h/j/k/l targets a window by geometry.
    /// Sidebar is on the left, ITEMS on the top-right, LOG at the bottom-right.
    fn navigate_panes(&mut self, c: char) {
        let target = match c {
            'h' => Focus::Sidebar,
            'j' => Focus::Log,
            // `l` leaves the sidebar for the items; `k` climbs back up from LOG.
            'l' | 'k' => Focus::List,
            _ => self.hover.unwrap_or(Focus::List),
        };
        let target = match target {
            Focus::Sidebar if !self.show_sidebar => Focus::List,
            Focus::Log if !self.show_log => Focus::List,
            other => other,
        };
        self.hover = Some(target);
        self.focus = target;
        self.message = format!("{} selected · Enter enter · ? help", target.label());
    }

    fn navigate_content(&mut self, c: char) {
        match self.focus {
            Focus::List => match c {
                'j' => self.move_selection(1),
                'k' => self.move_selection(-1),
                'h' => self.shift_view(-1),
                'l' => self.shift_view(1),
                _ => {}
            },
            Focus::Sidebar => match c {
                'j' | 'l' => self.shift_view(1),
                'k' | 'h' => self.shift_view(-1),
                _ => {}
            },
            Focus::Log => match c {
                'j' => self.log_step(1),
                'k' => self.log_step(-1),
                'h' => self.log_page(-1),
                'l' => self.log_page(1),
                _ => {}
            },
        }
    }

    fn navigate_move(&mut self, d: i32) {
        match self.layer {
            Layer::Panes => self.navigate_panes(if d > 0 { 'j' } else { 'k' }),
            Layer::Content => self.move_selection(d),
        }
    }

    /// Hiding the window that owns the cursor must not leave CONTENT pointing
    /// at an invisible pane: drop back to PANES with nothing selected.
    fn reset_hidden_focus(&mut self, fallback: Focus) {
        self.focus = fallback;
        self.layer = Layer::Panes;
        self.hover = None;
        self.visual_anchor = None;
        self.zoom = None;
    }

    /// `z` inside a window, or `prefix z` from anywhere: show that pane on its
    /// own, hiding every other one. Pressing it again restores the split.
    fn toggle_zoom(&mut self) {
        if self.zoom.take().is_some() {
            self.message = "split layout restored · z shows one window".into();
            return;
        }
        // In PANES the highlight decides; in CONTENT the window you are in.
        let target = if self.layer == Layer::Panes {
            self.hover.unwrap_or(self.focus)
        } else {
            self.focus
        };
        self.zoom = Some(target);
        self.message = format!("{} full screen · z or Esc twice restores", target.label());
    }

    fn toggle_log_pane(&mut self) {
        self.show_log = !self.show_log;
        if !self.show_log {
            if self.focus == Focus::Log {
                self.reset_hidden_focus(Focus::List);
            } else {
                self.zoom = None;
            }
        }
    }

    fn enter_pane(&mut self, f: Focus) {
        match f {
            Focus::Sidebar => self.show_sidebar = true,
            Focus::Log => self.show_log = true,
            Focus::List => {}
        }
        self.focus = f;
        self.hover = Some(f);
        self.layer = Layer::Content;
        self.visual_anchor = None;
        // A zoomed layout always shows the pane you are in: without this,
        // escaping to PANES and entering another window keeps the old one on
        // screen and the Enter looks like it did nothing.
        if self.zoom.is_some() {
            self.zoom = Some(f);
        }
        if f == Focus::Log {
            // Never re-enable follow just by entering the pane: a frozen view
            // (after j/k or while yanking) must stay exactly where it is.
            if self.log_follow {
                self.log_cursor = self.log_len().saturating_sub(1);
            }
            self.sync_log_view();
            self.message = format!(
                "LOG · {} · j/k line · h/l page · v select · y yank · Y all · G tail",
                if self.log_follow { "FOLLOW" } else { "SCROLL" }
            );
        } else {
            self.message = format!(
                "{} · j/k move · h/l workspace · Enter act · Esc panes",
                f.label()
            );
        }
    }

    fn escape(&mut self) {
        if self.visual_anchor.is_some() {
            self.visual_anchor = None;
            self.message = "visual selection cancelled".into();
            return;
        }
        if self.layer == Layer::Content {
            self.layer = Layer::Panes;
            self.hover = Some(self.focus);
            self.message =
                "PANES · h/j/k/l pick a window · Enter enter · Esc clears filters".into();
            return;
        }
        self.filter.clear();
        self.status_filter = None;
        self.message.clear();
        self.zoom = None;
        self.rebuild_rows();
    }

    // ---------------------------------------------------------------- log cursor

    fn log_len(&self) -> usize {
        self.current_log_id()
            .map(|id| self.log_len_of(&id))
            .unwrap_or(0)
    }

    fn log_jump(&mut self, line: usize, follow: bool) {
        self.log_follow = follow;
        self.log_cursor = line.min(self.log_len().saturating_sub(1));
        self.sync_log_view();
    }

    fn log_step(&mut self, d: i32) {
        let total = self.log_len();
        if total == 0 {
            self.message = "no log lines yet - start an app first".into();
            return;
        }
        self.log_follow = false;
        let cur = self.log_cursor.min(total - 1) as i64;
        self.log_cursor = (cur + d as i64).clamp(0, total as i64 - 1) as usize;
        self.sync_log_view();
    }

    fn log_page(&mut self, d: i32) {
        let step = self.log_view_height.max(1) as i32;
        self.log_step(d * step);
    }

    /// Keep the yank cursor inside the visible window (unless following the tail).
    fn sync_log_view(&mut self) {
        let total = self.log_len();
        let height = self.log_view_height.max(1);
        if total == 0 {
            self.log_scroll = 0;
            self.log_cursor = 0;
            return;
        }
        if self.log_follow {
            self.log_scroll = total.saturating_sub(height);
            self.log_cursor = total - 1;
            return;
        }
        self.log_cursor = self.log_cursor.min(total - 1);
        if self.log_cursor < self.log_scroll {
            self.log_scroll = self.log_cursor;
        } else if self.log_cursor >= self.log_scroll.saturating_add(height) {
            self.log_scroll = self.log_cursor + 1 - height;
        }
        self.log_scroll = self.log_scroll.min(total.saturating_sub(height));
    }

    fn toggle_visual(&mut self) {
        if self.visual_anchor.take().is_some() {
            self.message = "VISUAL off".into();
        } else {
            self.log_follow = false;
            self.visual_anchor = Some(self.log_cursor);
            self.message = "VISUAL · j/k extend · y yank · Esc cancel".into();
        }
    }

    pub fn log_selection(&self) -> Option<(usize, usize)> {
        self.visual_anchor
            .map(|a| (a.min(self.log_cursor), a.max(self.log_cursor)))
    }

    /// Pure range calculation: `Y` yanks everything, `v` yanks anchor..cursor,
    /// plain `y` yanks the cursor line. Kept separate so it is unit-testable.
    pub fn yank_text(&self, lines: &[String], all: bool) -> String {
        if lines.is_empty() {
            return String::new();
        }
        let last = lines.len() - 1;
        let (start, end) = if all {
            (0, last)
        } else if let Some((a, b)) = self.log_selection() {
            (a.min(last), b.min(last))
        } else {
            (self.log_cursor.min(last), self.log_cursor.min(last))
        };
        lines[start..=end].join("\n")
    }

    fn copy_log(&mut self, all: bool) {
        let Some(id) = self.current_log_id() else {
            self.message = "no log stream to yank - launch an app first".into();
            return;
        };
        // The buffer holds the raw stream; the clipboard gets what a terminal
        // would have shown, without colour codes.
        let lines: Vec<String> = self
            .logs_for(&id)
            .iter()
            .map(|l| crate::runner::plain_text(l))
            .collect();
        if lines.is_empty() {
            self.message = "log is empty".into();
            return;
        }
        let text = self.yank_text(&lines, all);
        let count = text.lines().count().max(1);
        self.clipboard = Some(text.clone());
        self.visual_anchor = None;
        self.message = match crate::clipboard::set_text(&text) {
            Ok(()) => format!("yanked {count} line(s) to the system clipboard"),
            Err(e) => match crate::clipboard::fallback_file(&text) {
                Ok(path) => format!("clipboard unavailable ({e}) - saved to {}", path.display()),
                Err(_) => format!("clipboard unavailable: {e}"),
            },
        };
    }

    fn current_config_count(&self) -> usize {
        self.current_row()
            .and_then(|r| match r.kind {
                RowKind::App {
                    project: Some(pi), ..
                } => Some(pi),
                _ => None,
            })
            .map(|pi| self.inv.projects[pi].launch_configs.len())
            .unwrap_or(0)
    }

    /// Legacy prefix focus shortcut: in the Vim model this enters the window.
    fn set_focus(&mut self, n: u8) {
        let f = match n {
            0 => Focus::Sidebar,
            2 => Focus::Log,
            _ => Focus::List,
        };
        self.enter_pane(f);
    }

    fn move_selection(&mut self, d: i32) {
        match self.focus {
            Focus::Sidebar => {
                let n = View::ALL.len();
                self.sidebar_selected = clamp_move(self.sidebar_selected, d, n);
                if let Some(v) = View::ALL.get(self.sidebar_selected) {
                    self.set_view(*v);
                }
            }
            Focus::List => {
                self.selected = clamp_move(self.selected, d, self.rows.len());
            }
            Focus::Log => self.log_step(d),
        }
    }

    fn shift_view(&mut self, d: i32) {
        let cur = View::ALL.iter().position(|v| *v == self.view).unwrap_or(0) as i32;
        let n = View::ALL.len() as i32;
        let next = (cur + d).rem_euclid(n) as usize;
        self.set_view(View::ALL[next]);
    }

    fn set_view(&mut self, v: View) {
        self.view = v;
        self.sidebar_selected = View::ALL.iter().position(|x| *x == v).unwrap_or(0);
        self.selected = 0;
        self.rebuild_rows();
    }

    fn primary_action(&mut self) {
        let Some(row) = self.current_row().cloned() else {
            return;
        };
        match row.kind {
            RowKind::Jdk(i) => {
                self.active_jdk = i;
                self.jdk_pinned = true;
                self.message = format!("JDK -> {}", self.jdk_label());
            }
            RowKind::Maven(i) => {
                self.active_maven = i;
                self.message = format!("Maven -> {}", self.maven_label());
            }
            RowKind::App { project, pid } => {
                if pid.is_some() || matches!(row.state, RunState::Running | RunState::Starting) {
                    self.selected_log_id = self.current_log_id();
                    self.enter_pane(Focus::Log);
                    self.log_follow = true;
                    self.sync_log_view();
                    self.message = "attached to live log · j/k scroll · y yank".into();
                    return;
                }
                match project {
                    Some(pi) => {
                        let _ = self.launch_project(pi);
                    }
                    None => self.message = "no project associated with this process".into(),
                }
            }
        }
    }

    pub fn launch_project(&mut self, pi: usize) -> Result<String> {
        if self.inv.jdks.is_empty() {
            anyhow::bail!("no JDK discovered - press r to rescan or set preferred_jdk in config");
        }
        let project = self.inv.projects[pi].clone();
        let Some(cfg) = self.config_of(pi).cloned() else {
            anyhow::bail!("no launch configuration for {}", project.name);
        };
        // Priority: explicit user/config JDK selection > IDEA run config JRE >
        // pom <java.version> match > active_jdk fallback.
        let jdk = if self.jdk_pinned {
            self.inv.jdks[self.active_jdk].clone()
        } else if let Some(jre) = cfg.jre_name.as_deref()
            && let Some(found) = self
                .inv
                .jdks
                .iter()
                .find(|j| j.name.to_lowercase().contains(&jre.to_lowercase()))
        {
            found.clone()
        } else {
            self.inv
                .jdks
                .iter()
                .find(|j| {
                    project
                        .java_version
                        .is_some_and(|w| jdk_major(&j.version) == Some(w))
                })
                .cloned()
                .unwrap_or_else(|| self.inv.jdks[self.active_jdk].clone())
        };
        let maven = self.inv.mavens.get(self.active_maven).cloned();
        match self
            .runner
            .launch(&project, &cfg, &jdk, maven.as_ref(), &self.cfg)
        {
            Ok(id) => {
                self.selected_log_id = Some(id.clone());
                self.enter_pane(Focus::Log);
                self.log_follow = true;
                self.sync_log_view();
                self.message = format!(
                    "launched {} with {} ({}) · log is live",
                    project.name, jdk.name, jdk.version
                );
                self.refresh_processes();
                Ok(id)
            }
            Err(e) => {
                self.message = format!("launch failed: {e:#}");
                Err(e)
            }
        }
    }

    pub fn stop_managed(&mut self, id: &str) -> Result<()> {
        let matched_java_pid = if let Some(m) = self.runner.managed.get(id) {
            self.inv.running.iter().find_map(|a| {
                let matches = a.pid == m.pid
                    || (self
                        .match_running_project(a)
                        .is_some_and(|pi| self.inv.projects[pi].path == m.project_path))
                    || a.work_dir.as_deref() == Some(&m.project_path);
                if matches { Some(a.pid) } else { None }
            })
        } else {
            None
        };
        let res = self.runner.stop(id);
        if let Some(jpid) = matched_java_pid {
            let _ = Runner::stop_external(jpid);
        }
        res
    }

    fn finish_stop_managed(&mut self, id: &str, was_dead: bool) {
        match self.stop_managed(id) {
            Ok(()) => {
                self.reset_log_stream();
                self.message = if was_dead {
                    format!("dismissed {id}")
                } else {
                    format!("stopped {id}")
                };
                self.refresh_processes();
            }
            Err(e) => self.message = format!("stop failed: {e}"),
        }
    }

    fn stop_selected(&mut self) {
        let Some(row) = self.current_row().cloned() else {
            return;
        };
        let RowKind::App { pid, .. } = row.kind else {
            self.message = "select an application first".into();
            return;
        };
        let Some(pid) = pid else {
            self.message = "not running".into();
            return;
        };
        if let Some(m) = self.runner.find_by_pid(pid) {
            let id = m.id.clone();
            let was_dead = matches!(m.state, RunState::Failed | RunState::Exited);
            if !was_dead {
                self.confirm_pid = Some(pid);
                self.mode = Mode::ConfirmStop;
                return;
            }
            self.finish_stop_managed(&id, true);
        } else {
            self.confirm_pid = Some(pid);
            self.mode = Mode::ConfirmStop;
        }
    }

    fn restart_selected(&mut self) {
        let Some(row) = self.current_row().cloned() else {
            return;
        };
        let RowKind::App {
            project: Some(pi),
            pid,
            ..
        } = row.kind
        else {
            self.message = "no project to restart".into();
            return;
        };
        if let Some(pid) = pid {
            if let Some(m) = self.runner.find_by_pid(pid) {
                let id = m.id.clone();
                let _ = self.stop_managed(&id);
            } else {
                self.message = "externally started process - stop it first (s)".into();
                return;
            }
        }
        if let Err(e) = self.launch_project(pi) {
            self.message = format!("restart failed: {e:#}");
        }
    }

    fn refresh_all(&mut self) {
        self.log_file_cache.borrow_mut().clear();
        match scan::collect(&self.cfg) {
            Ok(inv) => {
                self.inv = inv;
                self.active_jdk = self.active_jdk.min(self.inv.jdks.len().saturating_sub(1));
                self.active_maven = self
                    .active_maven
                    .min(self.inv.mavens.len().saturating_sub(1));
                self.rebuild_rows();
                self.message = format!("rescanned: {}", self.inv.summary());
            }
            Err(e) => self.message = format!("rescan failed: {e}"),
        }
    }

    fn refresh_processes(&mut self) {
        self.inv.running = scan::process::scan();
        self.log_file_cache.borrow_mut().clear();
        self.prune_stopped_file_logs();
        self.rebuild_rows();
    }

    fn reset_log_stream(&mut self) {
        self.selected_log_id = None;
        self.shown_log_id = None;
        self.visual_anchor = None;
        self.log_scroll = 0;
        self.log_cursor = 0;
    }

    fn prune_stopped_file_logs(&mut self) {
        self.file_logs.retain(|_, s| {
            s.pid
                .is_some_and(|p| self.inv.running.iter().any(|a| a.pid == p))
        });
        if let Some(id) = self.selected_log_id.as_deref()
            && id.starts_with("file://")
            && !self.is_live_file_log(id)
        {
            self.reset_log_stream();
        }
    }

    fn poll_file_logs(&mut self) {
        let Some(id) = self.current_log_id() else {
            return;
        };
        let Some(path_str) = id.strip_prefix("file://") else {
            return;
        };
        let row_pid = self.current_row().and_then(|r| match r.kind {
            RowKind::App { pid, .. } => pid,
            _ => None,
        });
        let now = Instant::now();
        let (should_read, path) = match self.file_logs.get(&id) {
            Some(state) => {
                let should = if state.last_read.elapsed() >= Duration::from_millis(250) {
                    let meta = std::fs::metadata(&state.path).ok();
                    let current_len = meta.as_ref().map(|m| m.len()).unwrap_or(0);
                    let current_mod = meta.and_then(|m| m.modified().ok());
                    current_len != state.last_len || current_mod != state.last_modified
                } else {
                    false
                };
                (should, state.path.clone())
            }
            None => (true, PathBuf::from(path_str)),
        };

        if should_read && let Ok(lines) = crate::scan::project::read_file_tail(&path, 2000) {
            let meta = std::fs::metadata(&path).ok();
            let current_len = meta.as_ref().map(|m| m.len()).unwrap_or(0);
            let current_mod = meta.and_then(|m| m.modified().ok());
            self.file_logs.insert(
                id.clone(),
                FileLogState {
                    pid: row_pid,
                    path,
                    last_read: now,
                    last_len: current_len,
                    last_modified: current_mod,
                    lines,
                },
            );
        } else if let Some(pid) = row_pid
            && let Some(state) = self.file_logs.get_mut(&id)
            && state.pid.is_none()
        {
            state.pid = Some(pid);
        }
    }

    pub fn tick(&mut self) {
        self.runner.poll();
        self.poll_file_logs();
        self.sync_log_target();
        // While following the tail, every new line scrolls into view. An app
        // that is logging hard must not leave the cursor stranded mid-buffer.
        // Frozen views (j/k, v, y) keep their position: copying is not a scroll.
        if self.log_follow {
            self.sync_log_view();
        }
        if self.last_proc_refresh.elapsed() >= Duration::from_secs(3) {
            self.refresh_processes();
            self.last_proc_refresh = Instant::now();
        }
    }

    /// Follow the cursor: when the selected row points at another stream, drop
    /// the old scroll position, visual selection and jump back to the live tail.
    fn sync_log_target(&mut self) {
        let id = self.current_log_id();
        if id == self.shown_log_id {
            return;
        }
        self.shown_log_id = id.clone();
        self.selected_log_id = id;
        self.visual_anchor = None;
        self.log_follow = true;
        self.sync_log_view();
    }

    fn handle_mouse(&mut self, m: MouseEvent) {
        let (x, y) = (m.column, m.row);
        match m.kind {
            MouseEventKind::ScrollUp | MouseEventKind::ScrollDown => {
                let d = if m.kind == MouseEventKind::ScrollUp {
                    -1
                } else {
                    1
                };
                // Mouse always acts on the pane under the pointer and enters it.
                if in_rect(self.hitboxes.log, x, y) {
                    if !self.is_log_content() {
                        self.enter_pane(Focus::Log);
                    }
                    self.log_step(d);
                } else if in_rect(self.hitboxes.list, x, y) {
                    if self.layer == Layer::Panes
                        || (self.layer == Layer::Content && self.focus != Focus::List)
                    {
                        self.enter_pane(Focus::List);
                    }
                    self.move_selection(d);
                } else if in_rect(self.hitboxes.sidebar, x, y) {
                    if self.layer == Layer::Panes
                        || (self.layer == Layer::Content && self.focus != Focus::Sidebar)
                    {
                        self.enter_pane(Focus::Sidebar);
                    }
                    self.move_selection(d);
                }
            }
            MouseEventKind::Down(MouseButton::Left) => {
                if in_rect(self.hitboxes.sidebar, x, y) {
                    if y > self.hitboxes.sidebar.y
                        && y + 1 < self.hitboxes.sidebar.y + self.hitboxes.sidebar.height
                    {
                        let idx = (y - self.hitboxes.sidebar.y - 1) as usize;
                        if let Some(v) = View::ALL.get(idx) {
                            self.set_view(*v);
                        }
                    }
                    self.enter_pane(Focus::Sidebar);
                } else if in_rect(self.hitboxes.list, x, y) {
                    if y > self.hitboxes.list.y
                        && y + 1 < self.hitboxes.list.y + self.hitboxes.list.height
                    {
                        let idx = self.list_scroll_offset + (y - self.hitboxes.list.y - 1) as usize;
                        if let Some(entry) = self.lines().get(idx)
                            && let Some(r) = entry.row
                        {
                            self.selected = r;
                        }
                    }
                    self.enter_pane(Focus::List);
                } else if in_rect(self.hitboxes.log, x, y) {
                    self.enter_pane(Focus::Log);
                }
            }
            _ => {}
        }
    }
    fn row_key(&self, r: &Row) -> String {
        match r.kind {
            RowKind::App {
                project: Some(pi),
                pid,
            } => format!(
                "{}::{}::{:?}",
                r.group,
                self.inv.projects[pi].path.display(),
                pid
            ),
            RowKind::App { project: None, pid } => {
                format!("{}::{}::{:?}", r.group, r.label, pid)
            }
            RowKind::Jdk(i) => format!("JDK::{}", self.inv.jdks[i].home.display()),
            RowKind::Maven(i) => format!("MAVEN::{}", self.inv.mavens[i].home.display()),
        }
    }
}

fn jdk_major(version: &str) -> Option<u32> {
    let mut parts = version.trim_matches('"').split('.');
    let mut major: u32 = parts.next()?.parse().ok()?;
    if major == 1 {
        // Legacy "1.8.0_302" style version strings.
        let minor: String = parts
            .next()?
            .chars()
            .take_while(|c| c.is_ascii_digit())
            .collect();
        major = minor.parse().ok()?;
    }
    Some(major)
}

fn clamp_move(cur: usize, d: i32, len: usize) -> usize {
    if len == 0 {
        return 0;
    }
    let n = len as i64;
    ((cur as i64 + d as i64).clamp(0, n - 1)) as usize
}

fn in_rect(r: Rect, x: u16, y: u16) -> bool {
    r.width > 0
        && r.height > 0
        && x >= r.x
        && x < r.x.saturating_add(r.width)
        && y >= r.y
        && y < r.y.saturating_add(r.height)
}
