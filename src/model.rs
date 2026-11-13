use std::path::PathBuf;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Origin {
    Env,
    Idea,
    System,
    Sdkman,
    Project,
}

impl Origin {
    pub fn label(self) -> &'static str {
        match self {
            Origin::Env => "ENV",
            Origin::Idea => "IDEA",
            Origin::System => "SYSTEM",
            Origin::Sdkman => "SDKMAN",
            Origin::Project => "PROJECT",
        }
    }
}

#[derive(Clone, Debug)]
pub struct Jdk {
    pub name: String,
    pub home: PathBuf,
    pub version: String,
    pub java: PathBuf,
    pub origin: Origin,
}

#[derive(Clone, Debug)]
pub struct Maven {
    pub name: String,
    pub home: PathBuf,
    pub version: String,
    pub executable: PathBuf,
    pub origin: Origin,
    pub wrapper: bool,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum BuildTool {
    Maven,
    Gradle,
}

impl BuildTool {
    pub fn label(self) -> &'static str {
        match self {
            BuildTool::Maven => "maven",
            BuildTool::Gradle => "gradle",
        }
    }
}

/// IDEA `SpringBootApplicationConfigurationType` run configuration, or a synthesized default.
#[derive(Clone, Debug)]
pub struct LaunchConfig {
    pub name: String,
    pub main_class: Option<String>,
    pub module_dir: Option<PathBuf>,
    pub vm_options: String,
    pub program_args: String,
    pub active_profiles: String,
    pub origin: String,
}

#[derive(Clone, Debug)]
pub struct Project {
    pub name: String,
    pub path: PathBuf,
    pub build: BuildTool,
    pub has_wrapper: bool,
    pub main_class: Option<String>,
    /// Major Java version declared by this module or one of its parent poms.
    pub java_version: Option<u32>,
    pub launch_configs: Vec<LaunchConfig>,
    pub origin: Origin,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RunState {
    Idle,
    Starting,
    Running,
    Failed,
    Exited,
}

impl RunState {
    /// Status marker: working / blocked / idle / done
    pub fn marker(self) -> &'static str {
        match self {
            RunState::Starting => "WORKING",
            RunState::Failed => "BLOCKED",
            RunState::Idle | RunState::Exited => "IDLE",
            RunState::Running => "DONE",
        }
    }

    pub fn glyph(self) -> &'static str {
        match self {
            RunState::Starting => "\u{25d0}",
            RunState::Failed => "\u{2716}",
            RunState::Idle => "\u{25cb}",
            RunState::Exited => "\u{2298}",
            RunState::Running => "\u{25cf}",
        }
    }

    pub fn short(self) -> &'static str {
        match self {
            RunState::Starting => "STARTING",
            RunState::Failed => "FAILED",
            RunState::Idle => "STOPPED",
            RunState::Exited => "EXITED",
            RunState::Running => "RUNNING",
        }
    }
}

#[derive(Clone, Debug)]
pub struct RunningApp {
    pub pid: u32,
    pub main_class: String,
    pub name: String,
    pub jdk_home: Option<PathBuf>,
    pub work_dir: Option<PathBuf>,
    pub ports: Vec<u16>,
    pub cmdline: String,
    pub managed: bool,
    pub origin: Origin,
}

#[derive(Clone, Debug, Default)]
pub struct Inventory {
    pub jdks: Vec<Jdk>,
    pub mavens: Vec<Maven>,
    pub projects: Vec<Project>,
    pub running: Vec<RunningApp>,
}

impl Inventory {
    pub fn summary(&self) -> String {
        format!(
            "JDK {} | Maven {} | Projects {} | Running {}",
            self.jdks.len(),
            self.mavens.len(),
            self.projects.len(),
            self.running.len()
        )
    }
}

/// One row in the main list / Goto picker. Indices point into `Inventory`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RowKind {
    App {
        project: Option<usize>,
        pid: Option<u32>,
    },
    Jdk(usize),
    Maven(usize),
}

#[derive(Clone, Debug)]
pub struct Row {
    pub kind: RowKind,
    pub group: String,
    pub label: String,
    pub origin: Origin,
    pub state: RunState,
    pub detail: String,
}

impl Row {
    pub fn search_text(&self) -> String {
        format!(
            "{} {} {} {}",
            self.group,
            self.label,
            self.origin.label(),
            self.detail
        )
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum View {
    All,
    Running,
    Projects,
    EnvTools,
    IdeaTools,
}

impl View {
    pub const ALL: [View; 5] = [
        View::All,
        View::Running,
        View::Projects,
        View::EnvTools,
        View::IdeaTools,
    ];

    pub fn title(self) -> &'static str {
        match self {
            View::All => "All",
            View::Running => "Running",
            View::Projects => "Projects",
            View::EnvTools => "Env toolchains",
            View::IdeaTools => "IDEA toolchains",
        }
    }
}
