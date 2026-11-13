use crate::app::{App, Focus, Layer, Mode, StatusFilter};
use crate::model::{Origin, RowKind, RunState, View};
use ratatui::layout::{Constraint, Layout, Rect};
use ratatui::prelude::*;
use ratatui::widgets::{
    Block, Borders, Clear, List, ListItem, ListState, Paragraph, Scrollbar, ScrollbarOrientation,
    ScrollbarState, Wrap,
};

const BORDER: Color = Color::DarkGray;
const ACCENT: Color = Color::Cyan;
const DIM: Color = Color::DarkGray;
const PICKED: Color = Color::Yellow;

/// PANES: the hovered window is yellow ("press Enter to enter"), everything
/// else stays dim. CONTENT: only the active window gets the accent border.
fn pane_border(app: &App, pane: Focus) -> Style {
    match app.layer {
        Layer::Content if app.focus == pane => Style::default().fg(ACCENT),
        Layer::Panes if app.hover == Some(pane) => Style::default().fg(PICKED),
        _ => Style::default().fg(BORDER),
    }
}

fn pane_title(app: &App, pane: Focus, base: &str) -> String {
    if app.layer == Layer::Panes && app.hover == Some(pane) {
        format!("{} [Enter] ", base.trim_end())
    } else {
        base.to_string()
    }
}

fn is_active(app: &App, pane: Focus) -> bool {
    app.layer == Layer::Content && app.focus == pane
}

fn state_color(s: RunState) -> Color {
    match s {
        RunState::Running => Color::Green,
        RunState::Starting => Color::Yellow,
        RunState::Failed => Color::Red,
        RunState::Exited => Color::DarkGray,
        RunState::Idle => Color::DarkGray,
    }
}

fn origin_color(o: Origin) -> Color {
    match o {
        Origin::Idea => Color::Magenta,
        Origin::Env => Color::Blue,
        Origin::Project => Color::Cyan,
        Origin::System => Color::DarkGray,
        Origin::Sdkman => Color::Yellow,
    }
}

pub fn render(f: &mut Frame<'_>, app: &mut App) {
    let full = f.area();
    app.hitboxes = Default::default();

    let outer = Layout::vertical([
        Constraint::Length(1),
        Constraint::Min(6),
        Constraint::Length(1),
    ])
    .split(full);
    let (header, body, footer) = (outer[0], outer[1], outer[2]);

    render_header(f, header, app);
    render_body(f, body, app);
    render_footer(f, footer, app);

    match app.mode {
        Mode::Help => render_help(f, full, app),
        Mode::Goto => render_goto(f, full, app),
        Mode::JdkPicker => render_picker(f, full, app, "SELECT JDK"),
        Mode::MavenPicker => render_picker(f, full, app, "SELECT MAVEN"),
        Mode::ConfigPicker => render_picker(f, full, app, "SELECT LAUNCH CONFIG"),
        Mode::ConfirmStop => render_confirm(
            f,
            full,
            "Stop external process?",
            "This process was started outside tspb (e.g. by IDEA).",
        ),
        Mode::ConfirmQuit => render_confirm(
            f,
            full,
            "Stop all managed apps and quit?",
            "Enter = stop everything   Esc = cancel (q detaches without stopping)",
        ),
        _ => {}
    }
}

fn render_header(f: &mut Frame<'_>, area: Rect, app: &App) {
    let prefix = format!("ctrl+{}", app.prefix_char);
    let prefix_span = if app.prefix_active {
        Span::styled(
            format!(" PREFIX [{}] ", prefix),
            Style::default().fg(Color::Black).bg(Color::Yellow).bold(),
        )
    } else {
        Span::styled(format!(" prefix {} ", prefix), Style::default().fg(DIM))
    };
    let view = Span::styled(
        format!(" {} ", app.view.title()),
        Style::default().fg(ACCENT).bold(),
    );
    let layer = match app.layer {
        Layer::Panes => Span::styled(
            " PANES ",
            Style::default().fg(Color::Black).bg(PICKED).bold(),
        ),
        Layer::Content => Span::styled(
            format!(
                " {}:{} ",
                if app.visual_anchor.is_some() {
                    "VISUAL"
                } else {
                    "CONTENT"
                },
                app.focus.label()
            ),
            Style::default().fg(Color::Black).bg(ACCENT).bold(),
        ),
    };
    let tools = Span::styled(
        format!(
            " JDK: {}  |  MVN: {}  |  CFG: {} ",
            app.jdk_label(),
            app.maven_label(),
            app.current_config_label()
        ),
        Style::default().fg(DIM),
    );
    let root = app
        .scan_root
        .file_name()
        .map(|s| s.to_string_lossy().into_owned())
        .unwrap_or_else(|| app.scan_root.to_string_lossy().into_owned());
    let mut spans = vec![
        Span::styled(
            " tspb ",
            Style::default().fg(Color::Black).bg(ACCENT).bold(),
        ),
        Span::raw(" "),
        view,
        layer,
        Span::styled(format!(" @{root} "), Style::default().fg(Color::Cyan)),
        prefix_span,
        tools,
    ];
    if let Some(sf) = app.status_filter {
        spans.push(Span::styled(
            format!(" [{}] ", sf.label()),
            Style::default().fg(Color::Yellow).bold(),
        ));
    }
    f.render_widget(Paragraph::new(Line::from(spans)), area);
}

fn render_body(f: &mut Frame<'_>, body: Rect, app: &mut App) {
    if let Some(z) = app.zoom {
        let area = Layout::vertical([Constraint::Min(1)]).split(body)[0];
        app.log_view_height = area.height.saturating_sub(2) as usize;
        match z {
            Focus::Sidebar => {
                app.hitboxes.sidebar = area;
                render_sidebar(f, area, app);
            }
            Focus::List => {
                app.hitboxes.list = area;
                render_list(f, area, app);
            }
            Focus::Log => {
                app.hitboxes.log = area;
                render_log(f, area, app);
            }
        }
        return;
    }

    let show_sidebar = app.show_sidebar;
    let cols = if show_sidebar {
        vec![Constraint::Length(26), Constraint::Min(20)]
    } else {
        vec![Constraint::Min(20)]
    };
    let chunks = Layout::horizontal(cols).split(body);
    let (sidebar_area, right) = if show_sidebar {
        (chunks[0], chunks[1])
    } else {
        (Rect::default(), chunks[0])
    };
    app.hitboxes.sidebar = sidebar_area;

    let mut rows = vec![Constraint::Min(5), Constraint::Length(5)];
    if app.show_log {
        rows.push(Constraint::Length(12));
    }
    let parts = Layout::vertical(rows).split(right);
    app.hitboxes.list = parts[0];
    app.hitboxes.detail = parts[1];
    app.hitboxes.log = if app.show_log {
        parts[2]
    } else {
        Rect::default()
    };
    app.log_view_height = app.hitboxes.log.height.saturating_sub(2) as usize;

    if show_sidebar {
        render_sidebar(f, sidebar_area, app);
    }
    render_list(f, parts[0], app);
    render_detail(f, parts[1], app);
    if app.show_log {
        render_log(f, parts[2], app);
    }
}

fn render_sidebar(f: &mut Frame<'_>, area: Rect, app: &App) {
    let mut lines: Vec<Line> = Vec::new();
    for v in View::ALL.iter() {
        let active = *v == app.view;
        let marker = if active { "▸" } else { " " };
        let style = if active {
            Style::default().fg(ACCENT).bold()
        } else {
            Style::default()
        };
        lines.push(Line::from(vec![
            Span::styled(format!("{marker} "), style),
            Span::styled(format!("{:<16}", v.title()), style),
            Span::styled(
                format!("{:>3}", app.view_count(*v)),
                Style::default().fg(DIM),
            ),
        ]));
    }
    lines.push(Line::from(""));
    lines.push(Line::from(Span::styled(
        "TOOLCHAIN",
        Style::default().fg(DIM).bold(),
    )));
    lines.push(Line::from(Span::styled(
        format!("  {}", truncate(&app.jdk_label(), 22)),
        Style::default().fg(Color::Green),
    )));
    lines.push(Line::from(Span::styled(
        format!("  {}", truncate(&app.maven_label(), 22)),
        Style::default().fg(Color::Blue),
    )));
    lines.push(Line::from(""));
    lines.push(Line::from(Span::styled(
        "STATUS",
        Style::default().fg(DIM).bold(),
    )));
    for (key, label) in [
        ('w', "WORKING"),
        ('b', "BLOCKED"),
        ('i', "IDLE"),
        ('d', "DONE"),
    ] {
        let on = app.status_filter.map(|f| match f {
            StatusFilter::Working => 'w',
            StatusFilter::Blocked => 'b',
            StatusFilter::Idle => 'i',
            StatusFilter::Done => 'd',
        }) == Some(key);
        lines.push(Line::from(vec![
            Span::styled(format!("  {key} "), Style::default().fg(Color::Yellow)),
            Span::styled(
                label,
                if on {
                    Style::default().bold()
                } else {
                    Style::default().fg(DIM)
                },
            ),
        ]));
    }

    let block = Block::bordered()
        .title(pane_title(app, Focus::Sidebar, " WORKSPACES "))
        .border_style(pane_border(app, Focus::Sidebar));
    f.render_widget(Paragraph::new(lines).block(block), area);
}

fn render_list(f: &mut Frame<'_>, area: Rect, app: &App) {
    let width = area.width.saturating_sub(4) as usize;
    let mut items: Vec<ListItem> = Vec::new();
    for entry in app.lines() {
        match entry.row {
            None => {
                items.push(ListItem::new(Line::from(Span::styled(
                    format!("── {} ──", entry.group),
                    Style::default().fg(ACCENT).bold(),
                ))));
            }
            Some(i) => {
                let r = &app.rows[i];
                let c = state_color(r.state);
                let label_w = 30usize.min(width);
                let rest_w = width.saturating_sub(label_w + 24);
                items.push(ListItem::new(Line::from(vec![
                    Span::styled(format!("{} ", r.state.glyph()), Style::default().fg(c)),
                    Span::styled(format!("{:<8}", r.state.marker()), Style::default().fg(c)),
                    Span::styled(
                        format!("{:<width$}", truncate(&r.label, label_w), width = label_w),
                        Style::default(),
                    ),
                    Span::styled(
                        format!(" {:<7}", r.origin.label()),
                        Style::default().fg(origin_color(r.origin)),
                    ),
                    Span::styled(truncate(&r.detail, rest_w), Style::default().fg(DIM)),
                ])));
            }
        }
    }
    let focus = is_active(app, Focus::List);
    let title = format!(
        " ITEMS ({}){}{} ",
        app.rows.len(),
        if app.filter.is_empty() {
            String::new()
        } else {
            format!(" /{}", app.filter)
        },
        if app.mode == Mode::Filter {
            " [typing]"
        } else {
            ""
        },
    );
    let list = List::new(items)
        .block(
            Block::bordered()
                .title(pane_title(app, Focus::List, &title))
                .border_style(pane_border(app, Focus::List)),
        )
        .highlight_style(Style::default().add_modifier(Modifier::REVERSED))
        .highlight_symbol("▸ ");
    let mut state = ListState::default();
    // In PANES no window is active, so no list row is highlighted either.
    if focus {
        state.select(app.selected_line());
    }
    f.render_stateful_widget(list, area, &mut state);
}

fn render_detail(f: &mut Frame<'_>, area: Rect, app: &App) {
    let selected = app.current_row();
    let (label, state, origin, detail) = selected
        .map(|r| (r.label.as_str(), r.state, r.origin, r.detail.as_str()))
        .unwrap_or(("nothing selected", RunState::Idle, Origin::System, ""));
    let project_path = selected
        .and_then(|r| match r.kind {
            RowKind::App {
                project: Some(pi), ..
            } => Some(app.inv.projects[pi].path.display().to_string()),
            _ => None,
        })
        .unwrap_or_default();
    let main_class = selected
        .and_then(|r| match r.kind {
            RowKind::App {
                project: Some(pi), ..
            } => app.inv.projects[pi].main_class.clone(),
            _ => None,
        })
        .unwrap_or_else(|| "-".into());

    let lines = vec![
        Line::from(vec![
            Span::styled(" SELECTED  ", Style::default().fg(DIM)),
            Span::styled(label, Style::default().bold()),
            Span::styled(
                format!("  {} {}", state.glyph(), state.short()),
                Style::default().fg(state_color(state)),
            ),
            Span::styled(
                format!("  [{}]", origin.label()),
                Style::default().fg(origin_color(origin)),
            ),
        ]),
        Line::from(vec![
            Span::styled(" PATH/MAIN ", Style::default().fg(DIM)),
            Span::styled(
                truncate(
                    &if project_path.is_empty() {
                        detail.to_string()
                    } else {
                        project_path
                    },
                    70,
                ),
                Style::default(),
            ),
            Span::styled(
                format!("  {}", truncate(&main_class, 36)),
                Style::default().fg(DIM),
            ),
        ]),
        Line::from(vec![
            Span::styled(" TOOLCHAIN ", Style::default().fg(DIM)),
            Span::styled(
                format!("JDK {}  ", app.jdk_label()),
                Style::default().fg(Color::Green),
            ),
            Span::styled(
                format!("MVN {}  ", app.maven_label()),
                Style::default().fg(Color::Blue),
            ),
            Span::styled(
                format!("CFG {}", app.current_config_label()),
                Style::default().fg(Color::Magenta),
            ),
        ]),
    ];
    f.render_widget(
        Paragraph::new(lines).block(
            Block::bordered()
                .title(" DETAIL ")
                .border_style(Style::default().fg(BORDER)),
        ),
        area,
    );
}

fn render_log(f: &mut Frame<'_>, area: Rect, app: &App) {
    let active = is_active(app, Focus::Log);
    let height = area.height.saturating_sub(2) as usize;
    let id = app.current_log_id();
    let mut lines: Vec<Line> = Vec::new();
    let mut title = String::from(" LOG ");
    let mut total = 0usize;

    if let Some(id) = &id {
        let raw = app.runner.logs_for(id);
        total = raw.len();
        title = format!(
            " LOG  pid {} ",
            app.runner.managed.get(id).map(|m| m.pid).unwrap_or(0)
        );
        let start = if app.log_follow {
            total.saturating_sub(height)
        } else {
            app.log_scroll.min(total.saturating_sub(height))
        };
        let selection = app.log_selection();
        for (offset, text) in raw.iter().skip(start).take(height).enumerate() {
            let abs = start + offset;
            let lower = text.to_lowercase();
            let color = if lower.contains("error")
                || lower.contains("exception")
                || lower.contains("failed")
            {
                Color::Red
            } else if lower.contains("warn") {
                Color::Yellow
            } else if lower.contains("started") || lower.contains("build success") {
                Color::Green
            } else if text.contains("$ ") {
                Color::Cyan
            } else {
                Color::Reset
            };
            let mut style = Style::default().fg(color);
            if active && selection.is_some_and(|(a, b)| abs >= a && abs <= b) {
                style = style.bg(Color::DarkGray).bold();
            }
            // The yank cursor is reversed so it is obvious which line `y` copies.
            if active && abs == app.log_cursor {
                style = style.add_modifier(Modifier::REVERSED);
            }
            let marker = if active && abs == app.log_cursor {
                Span::styled("▸", Style::default().fg(PICKED))
            } else {
                Span::raw(" ")
            };
            lines.push(Line::from(vec![marker, Span::styled(text.clone(), style)]));
        }
    } else if let Some(row) = app.current_row() {
        if let RowKind::App { pid: Some(pid), .. } = row.kind {
            lines.push(Line::from(Span::styled(
                format!("pid {pid} was started outside tspb (e.g. from IDEA).",),
                Style::default().fg(Color::Yellow),
            )));
            lines.push(Line::from(Span::styled(
                "Its stdout/stderr belongs to that JVM's console and cannot be captured here.",
                Style::default().fg(DIM),
            )));
            lines.push(Line::from(Span::styled(
                "Press s to stop it, or select the matching project and press Enter to relaunch with live logs.",
                Style::default().fg(DIM),
            )));
        } else {
            lines.push(Line::from(Span::styled(
                "Select a project and press Enter to launch it. Live stdout/stderr appears here.",
                Style::default().fg(DIM),
            )));
        }
    } else {
        lines.push(Line::from(Span::styled(
            "no log stream",
            Style::default().fg(DIM),
        )));
    }

    let mode = if app.visual_anchor.is_some() {
        "VISUAL"
    } else if app.log_follow {
        "FOLLOW"
    } else {
        "SCROLL"
    };
    let pos = if total == 0 {
        0
    } else {
        app.log_cursor.min(total - 1) + 1
    };
    let hint = if active {
        format!(
            " {mode} {pos}/{total} · j/k line · h/l page · v select · y yank · Y all · g/G ends · Esc panes "
        )
    } else {
        format!(" {mode} {pos}/{total} · pick LOG (j) then Enter to scroll / copy ")
    };
    let block = Block::bordered()
        .title(pane_title(app, Focus::Log, &title))
        .title_bottom(Line::from(Span::styled(hint, Style::default().fg(DIM))))
        .border_style(pane_border(app, Focus::Log));
    f.render_widget(
        Paragraph::new(lines)
            .block(block)
            .wrap(Wrap { trim: false }),
        area,
    );

    // Scrollbar sits on the last content column, leaving the border untouched.
    if total > 0 && area.width > 4 && area.height > 3 {
        let track = Rect {
            x: area.x + area.width.saturating_sub(2),
            y: area.y + 1,
            width: 1,
            height: area.height.saturating_sub(2),
        };
        let mut state = ScrollbarState::new(total)
            .position(app.log_cursor.min(total - 1))
            .viewport_content_length(height.max(1));
        let bar = Scrollbar::new(ScrollbarOrientation::VerticalRight)
            .thumb_style(Style::default().fg(ACCENT))
            .track_style(Style::default().fg(BORDER))
            .begin_symbol(Some("↑"))
            .end_symbol(Some("↓"));
        f.render_stateful_widget(bar, track, &mut state);
    }
}

fn render_footer(f: &mut Frame<'_>, area: Rect, app: &App) {
    let hint = match app.layer {
        Layer::Panes => {
            " PANES: h/j/k/l pick window · Enter enter · ? help · r rescan · q detach · Esc clear "
        }
        Layer::Content => match app.focus {
            Focus::List => {
                " ITEMS: j/k move · h/l workspace · Enter launch/attach · / filter · s stop · R restart · Esc panes "
            }
            Focus::Sidebar => " WORKSPACES: j/k or h/l switch view · Enter act · Esc panes ",
            Focus::Log => {
                " LOG: j/k line · h/l page · v select · y yank line · Y yank all · g top · G tail · Esc panes "
            }
        },
    };
    let keys = Span::styled(hint, Style::default().fg(DIM));
    let msg_style = if app.message.starts_with("launch failed")
        || app.message.starts_with("stop failed")
        || app.message.starts_with("rescan failed")
    {
        Style::default().fg(Color::Red)
    } else {
        Style::default().fg(Color::Yellow)
    };
    let msg = Span::styled(format!(" {} ", truncate(&app.message, 40)), msg_style);
    f.render_widget(Paragraph::new(Line::from(vec![keys, msg])), area);
}

// -------------------------------------------------------------------- overlays

fn centered(area: Rect, width: u16, height: u16) -> Rect {
    let w = width.min(area.width.saturating_sub(2)).max(20);
    let h = height.min(area.height.saturating_sub(2)).max(4);
    Rect {
        x: area.x + (area.width.saturating_sub(w)) / 2,
        y: area.y + (area.height.saturating_sub(h)) / 2,
        width: w,
        height: h,
    }
}

fn render_help(f: &mut Frame<'_>, full: Rect, app: &App) {
    let area = centered(full, 88, 32);
    f.render_widget(Clear, area);
    let prefix = format!("ctrl+{}", app.prefix_char);
    let text = vec![
        h("KEYBOARD - Vim-style two-layer model"),
        s(""),
        h("  PANES (default: NO window is selected)"),
        k("h/j/k/l (arrows)", "pick WORKSPACES / LOG / ITEMS window"),
        k("Enter", "enter the picked window -> CONTENT layer"),
        k("Esc", "clear filters / status / zoom"),
        s(""),
        h("  CONTENT - h/j/k/l act INSIDE the active window"),
        k("ITEMS j/k, h/l", "move selection / switch workspace"),
        k("WORKSPACES j/k/h/l", "switch workspace (view)"),
        k("LOG j/k, h/l", "move yank cursor / page up-down"),
        k("LOG g / G / End", "first line / live tail"),
        k("Esc", "back to PANES (cancels VISUAL first)"),
        s(""),
        h("  LOG: copy + scrollbar"),
        k("v", "VISUAL: anchor a line selection (j/k extends)"),
        k("y / Y", "yank line(s) / whole log to system clipboard"),
        k(
            "no clipboard",
            "text is saved to a temp file, message shows path",
        ),
        k("right scrollbar", "cursor position over total log lines"),
        s(""),
        h("  ITEM ACTIONS"),
        k("Enter", "launch / attach log / pick toolchain"),
        k("/ w b i d a s R", "filter, status filters, stop, restart"),
        k("J / M / c / Tab", "JDK / Maven / launch config pickers"),
        k("r ? q Q Ctrl+C", "rescan, help, detach, stop-all, exit"),
        s(""),
        h(&format!(
            "  PREFIX {prefix}: h/l/j jump into a window; ? q Q g v x z b r [ still work"
        )),
        s("  ● DONE running  ◐ WORKING starting  ✖ BLOCKED failed  ○ IDLE stopped"),
        s("  Esc or Enter closes this panel"),
    ];
    let height = area.height.saturating_sub(2) as usize;
    let visible: Vec<Line> = text
        .into_iter()
        .skip(app.help_scroll)
        .take(height)
        .collect();
    let block = Block::bordered()
        .title(" HELP ")
        .border_style(Style::default().fg(ACCENT));
    f.render_widget(
        Paragraph::new(visible)
            .block(block)
            .wrap(Wrap { trim: false }),
        area,
    );
}

fn h(text: &str) -> Line<'static> {
    Line::from(Span::styled(
        text.to_string(),
        Style::default().fg(ACCENT).bold(),
    ))
}
fn s(text: &str) -> Line<'static> {
    Line::from(Span::styled(text.to_string(), Style::default().fg(DIM)))
}
fn k(key: &str, desc: &str) -> Line<'static> {
    Line::from(vec![
        Span::styled(format!("  {:<22}", key), Style::default().fg(Color::Yellow)),
        Span::styled(desc.to_string(), Style::default()),
    ])
}

fn render_goto(f: &mut Frame<'_>, full: Rect, app: &App) {
    let area = centered(full, 78, 24);
    f.render_widget(Clear, area);
    let items = app.goto_items();
    let height = area.height.saturating_sub(4) as usize;
    let list: Vec<ListItem> = items
        .iter()
        .skip(app.picker_selected.saturating_sub(height.saturating_sub(1)))
        .take(height)
        .map(|(label, _)| ListItem::new(Line::from(label.clone())))
        .collect();
    let block = Block::bordered()
        .title(format!(
            " GOTO  /{} ({} matches) ",
            app.goto_filter,
            items.len()
        ))
        .title_bottom(" Enter jump · Up/Down move · type to search · Esc cancel ")
        .border_style(Style::default().fg(ACCENT));
    let widget = List::new(list)
        .block(block)
        .highlight_style(Style::default().add_modifier(Modifier::REVERSED))
        .highlight_symbol("▸ ");
    let mut state = ListState::default();
    let offset = app.picker_selected.saturating_sub(height.saturating_sub(1));
    state.select(Some(app.picker_selected.saturating_sub(offset)));
    f.render_stateful_widget(widget, area, &mut state);
}

fn render_picker(f: &mut Frame<'_>, full: Rect, app: &App, title: &str) {
    let area = centered(full, 76, 18);
    f.render_widget(Clear, area);
    let items: Vec<String> = match app.mode {
        Mode::JdkPicker => app
            .inv
            .jdks
            .iter()
            .map(|j| {
                format!(
                    "{}  {}  [{}]  {}",
                    j.name,
                    j.version,
                    j.origin.label(),
                    j.home.display()
                )
            })
            .collect(),
        Mode::MavenPicker => app
            .inv
            .mavens
            .iter()
            .map(|m| {
                format!(
                    "{}  {}  [{}]  {}",
                    m.name,
                    m.version,
                    m.origin.label(),
                    m.home.display()
                )
            })
            .collect(),
        Mode::ConfigPicker => app
            .current_row()
            .and_then(|r| match r.kind {
                RowKind::App {
                    project: Some(pi), ..
                } => Some(&app.inv.projects[pi]),
                _ => None,
            })
            .map(|p| {
                p.launch_configs
                    .iter()
                    .map(|c| {
                        format!(
                            "{}  [{}]  main={}",
                            c.name,
                            c.origin,
                            c.main_class.as_deref().unwrap_or("auto")
                        )
                    })
                    .collect()
            })
            .unwrap_or_default(),
        _ => Vec::new(),
    };
    let height = area.height.saturating_sub(2) as usize;
    let offset = app.picker_selected.saturating_sub(height.saturating_sub(1));
    let list: Vec<ListItem> = items
        .iter()
        .skip(offset)
        .take(height)
        .map(|s| {
            ListItem::new(Line::from(truncate(
                s,
                area.width.saturating_sub(4) as usize,
            )))
        })
        .collect();
    let widget = List::new(list)
        .block(
            Block::bordered()
                .title(format!(" {title} "))
                .border_style(Style::default().fg(ACCENT)),
        )
        .highlight_style(Style::default().add_modifier(Modifier::REVERSED))
        .highlight_symbol("▸ ");
    let mut state = ListState::default();
    state.select(Some(app.picker_selected.saturating_sub(offset)));
    f.render_stateful_widget(widget, area, &mut state);
}

fn render_confirm(f: &mut Frame<'_>, full: Rect, title: &str, body: &str) {
    let area = centered(full, 62, 7);
    f.render_widget(Clear, area);
    let text = vec![
        Line::from(Span::styled(
            title.to_string(),
            Style::default().fg(Color::Yellow).bold(),
        )),
        Line::from(""),
        Line::from(Span::styled(body.to_string(), Style::default().fg(DIM))),
        Line::from(""),
        Line::from(Span::styled(
            "  Enter = confirm      Esc = cancel  ",
            Style::default().fg(ACCENT),
        )),
    ];
    f.render_widget(
        Paragraph::new(text).block(
            Block::bordered()
                .borders(Borders::ALL)
                .border_style(Style::default().fg(Color::Yellow)),
        ),
        area,
    );
}

fn truncate(s: &str, max: usize) -> String {
    if max == 0 {
        return String::new();
    }
    let chars: Vec<char> = s.chars().collect();
    if chars.len() <= max {
        return s.to_string();
    }
    let mut out: String = chars.iter().take(max.saturating_sub(1)).collect();
    out.push('…');
    out
}
