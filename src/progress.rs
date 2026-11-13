//! Startup scan progress.
//!
//! `scan::collect` is filesystem-bound and can take seconds on a large tree or
//! a machine with many JDK/Maven installs. The TUI therefore runs the scan on a
//! worker thread and polls a shared [`ScanProgress`] to draw a gauge until the
//! [`Inventory`](crate::model::Inventory) is ready.
//!
//! Scanner code only talks to the cheap [`Reporter`] handle, so the same code
//! path stays usable without a UI (`--scan-only`, `--launch`, tests).

use ratatui::Frame;
use ratatui::layout::{Alignment, Constraint, Layout, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Clear, Gauge, Paragraph};
use std::sync::{Arc, Mutex};
use std::time::Instant;

/// Stage labels in execution order; the gauge is segmented by these.
pub const STAGES: [&str; 4] = [
    "Reading IntelliJ toolchains",
    "Scanning JDK / Maven installs",
    "Indexing Spring Boot projects",
    "Detecting running apps",
];
pub const STAGE_IDEA: usize = 0;
pub const STAGE_ENV: usize = 1;
pub const STAGE_PROJECTS: usize = 2;
pub const STAGE_PROCESSES: usize = 3;

const ACCENT: Color = Color::Cyan;
const DIM: Color = Color::DarkGray;
const SPINNER: [&str; 10] = ["⠋", "⠙", "⠹", "⠸", "⠼", "⠴", "⠦", "⠧", "⠇", "⠏"];
/// Compact stage names for the checklist row under the gauge.
const SHORT: [&str; 4] = ["idea", "toolchains", "projects", "processes"];

#[derive(Clone, Debug)]
pub struct ScanProgress {
    /// Index into [`STAGES`]; `STAGES.len()` once the scan finished.
    pub stage: usize,
    pub stage_label: String,
    /// What the current stage is looking at (e.g. the root being walked).
    pub detail: String,
    pub jdks: usize,
    pub mavens: usize,
    pub projects: usize,
    pub running: usize,
    /// Directories visited by the project walk.
    pub dirs: usize,
    pub started: Instant,
    stage_started: Instant,
}

impl Default for ScanProgress {
    fn default() -> Self {
        Self::new()
    }
}

impl ScanProgress {
    pub fn new() -> Self {
        let now = Instant::now();
        Self {
            stage: 0,
            stage_label: STAGES[0].to_string(),
            detail: String::new(),
            jdks: 0,
            mavens: 0,
            projects: 0,
            running: 0,
            dirs: 0,
            started: now,
            stage_started: now,
        }
    }

    fn begin(&mut self, stage: usize) {
        self.stage = stage.min(STAGES.len());
        self.stage_label = STAGES.get(stage).copied().unwrap_or("Ready").to_string();
        self.detail.clear();
        self.dirs = 0;
        self.stage_started = Instant::now();
    }

    /// Overall completion, 0.0..=1.0.
    ///
    /// Only the total stage count is known up front, so the bar is driven by
    /// completed stages. Within a stage it creeps forward: the project walk is
    /// asymptotic on the number of directories visited (there is no way to know
    /// the total without a second traversal), the other stages ease out over
    /// roughly two seconds so the UI still shows life during `sysinfo` refreshes.
    pub fn ratio(&self) -> f64 {
        let total = STAGES.len() as f64;
        if self.stage >= STAGES.len() {
            return 1.0;
        }
        let intra = match self.stage {
            STAGE_PROJECTS => {
                let d = self.dirs as f64;
                (d / (d + 400.0)).min(0.92)
            }
            _ => {
                let t = self.stage_started.elapsed().as_secs_f64();
                (0.9 * (1.0 - (-t / 2.0).exp())).min(0.9)
            }
        };
        ((self.stage as f64 + intra) / total).clamp(0.0, 0.99)
    }
}

/// Write-only progress channel handed to the scanner. Cloning it costs one
/// `Arc` bump; without a UI it is just a no-op.
#[derive(Clone, Default)]
pub struct Reporter(Option<Arc<Mutex<ScanProgress>>>);

impl Reporter {
    /// Reporter used by non-interactive paths (`--scan-only`, rescans, tests).
    pub fn silent() -> Self {
        Self(None)
    }

    pub fn new(shared: Arc<Mutex<ScanProgress>>) -> Self {
        Self(Some(shared))
    }

    fn with(&self, f: impl FnOnce(&mut ScanProgress)) {
        let Some(shared) = self.0.as_ref() else {
            return;
        };
        if let Ok(mut guard) = shared.lock() {
            f(&mut guard);
        }
    }

    pub fn stage(&self, stage: usize) {
        self.with(|p| p.begin(stage));
    }

    pub fn detail(&self, text: impl Into<String>) {
        let text = text.into();
        self.with(|p| p.detail = text);
    }

    pub fn dirs(&self, n: usize) {
        self.with(|p| p.dirs = n);
    }

    pub fn jdks(&self, n: usize) {
        self.with(|p| p.jdks = n);
    }

    pub fn mavens(&self, n: usize) {
        self.with(|p| p.mavens = n);
    }

    pub fn projects(&self, n: usize) {
        self.with(|p| p.projects = n);
    }

    pub fn running(&self, n: usize) {
        self.with(|p| p.running = n);
    }

    /// Mark every stage complete; the main UI is about to replace this screen.
    pub fn finish(&self) {
        self.with(|p| {
            p.stage = STAGES.len();
            p.stage_label = "Ready".into();
            p.detail.clear();
        });
    }
}

/// Splash screen shown while the worker thread scans.
pub fn render(f: &mut Frame<'_>, p: &ScanProgress) {
    let area = f.area();
    if area.width < 24 || area.height < 6 {
        let spin = spinner(p);
        let line = Line::from(Span::styled(
            format!("{spin} {}", p.stage_label),
            Style::default().fg(ACCENT),
        ));
        f.render_widget(Paragraph::new(line).alignment(Alignment::Center), area);
        return;
    }

    let width = area.width.saturating_sub(4).clamp(24, 72);
    let height = 7u16.min(area.height);
    let rect = Rect::new(
        area.x + (area.width - width) / 2,
        area.y + (area.height - height) / 2,
        width,
        height,
    );
    f.render_widget(Clear, rect);

    let block = Block::default()
        .borders(Borders::ALL)
        .border_style(Style::default().fg(DIM))
        .title(Line::from(vec![
            Span::styled(
                " tspb ",
                Style::default().fg(ACCENT).add_modifier(Modifier::BOLD),
            ),
            Span::styled("· indexing workspace ", Style::default().fg(DIM)),
        ]))
        .title_bottom(
            Line::from(Span::styled(" Esc cancel ", Style::default().fg(DIM)))
                .alignment(Alignment::Right),
        );
    let inner = block.inner(rect);
    f.render_widget(block, rect);
    if inner.height < 3 {
        return;
    }

    let rows = Layout::vertical([
        Constraint::Length(1), // stage line
        Constraint::Length(1), // gauge
        Constraint::Length(1), // counters + elapsed
        Constraint::Length(1), // stage checklist
        Constraint::Min(0),    // directory being walked
    ])
    .split(inner);

    // Stage line: " ⠹ Indexing Spring Boot projects      3 / 4 ".
    let step = (p.stage + 1).min(STAGES.len());
    let left_label = format!(" {} {}", spinner(p), p.stage_label);
    let right_label = format!("{step} / {} ", STAGES.len());
    f.render_widget(
        Paragraph::new(columns(
            &left_label,
            &right_label,
            inner.width,
            Style::default().fg(ACCENT),
            Style::default().fg(DIM),
        )),
        rows[0],
    );

    let ratio = p.ratio();
    let gauge = Gauge::default()
        .gauge_style(Style::default().fg(ACCENT).bg(Color::Black))
        .ratio(ratio)
        .label(format!("{:>3}%", (ratio * 100.0).round() as u32));
    f.render_widget(gauge, rows[1]);

    let mut counters: Vec<String> = Vec::new();
    if p.jdks > 0 {
        counters.push(format!("jdk {}", p.jdks));
    }
    if p.mavens > 0 {
        counters.push(format!("maven {}", p.mavens));
    }
    if p.projects > 0 {
        counters.push(format!("projects {}", p.projects));
    }
    if p.dirs > 0 {
        counters.push(format!("dirs {}", p.dirs));
    }
    if p.running > 0 {
        counters.push(format!("running {}", p.running));
    }
    let counters = if counters.is_empty() {
        p.stage_label.to_lowercase()
    } else {
        counters.join(" · ")
    };
    f.render_widget(
        Paragraph::new(columns(
            &format!(" {counters}"),
            &format!("{:.1}s ", p.started.elapsed().as_secs_f64()),
            inner.width,
            Style::default().fg(DIM),
            Style::default().fg(DIM),
        )),
        rows[2],
    );

    // Checklist: ✓ done, ▸ running, · pending.
    let mut spans: Vec<Span<'static>> = vec![Span::raw(" ")];
    for (i, name) in SHORT.iter().enumerate() {
        let (mark, style) = if i < p.stage {
            ("✓", Style::default().fg(Color::Green))
        } else if i == p.stage {
            (
                "▸",
                Style::default().fg(ACCENT).add_modifier(Modifier::BOLD),
            )
        } else {
            ("·", Style::default().fg(DIM))
        };
        spans.push(Span::styled(format!("{mark} {name}  "), style));
    }
    f.render_widget(Paragraph::new(Line::from(spans)), rows[3]);

    // Detail line: the directory currently being walked.
    if !p.detail.is_empty() {
        let avail = (inner.width as usize).saturating_sub(2);
        f.render_widget(
            Paragraph::new(Span::styled(
                format!(" {}", clip(&p.detail, avail)),
                Style::default().fg(DIM),
            )),
            rows[4],
        );
    }
}

/// One row with a left and a right label, gap computed from the pane width.
fn columns(
    left: &str,
    right: &str,
    width: u16,
    left_style: Style,
    right_style: Style,
) -> Line<'static> {
    let pad = (width as usize).saturating_sub(left.chars().count() + right.chars().count());
    Line::from(vec![
        Span::styled(left.to_string(), left_style),
        Span::raw(" ".repeat(pad)),
        Span::styled(right.to_string(), right_style),
    ])
}

fn spinner(p: &ScanProgress) -> &'static str {
    let i = (p.started.elapsed().as_millis() / 90) as usize % SPINNER.len();
    SPINNER[i]
}

fn clip(text: &str, width: usize) -> String {
    if width == 0 {
        return String::new();
    }
    if text.chars().count() <= width {
        return text.to_string();
    }
    let mut out: String = text.chars().take(width.saturating_sub(1)).collect();
    out.push('…');
    out
}
