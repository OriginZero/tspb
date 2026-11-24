mod app;
mod clipboard;
mod config;
mod event;
mod model;
mod progress;
mod runner;
mod scan;
mod ui;
mod update;

#[cfg(test)]
mod tests;

use anyhow::{Result, bail};
use crossterm::event::{DisableMouseCapture, EnableMouseCapture, poll, read};
use crossterm::execute;
use crossterm::terminal::{
    EnterAlternateScreen, LeaveAlternateScreen, disable_raw_mode, enable_raw_mode,
};
use model::RunState;
use ratatui::Terminal;
use ratatui::backend::CrosstermBackend;
use std::io::{Stdout, Write};
use std::path::PathBuf;
use std::sync::mpsc::{self, TryRecvError};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

fn main() -> Result<()> {
    let args: Vec<String> = std::env::args().skip(1).collect();
    if args.iter().any(|a| a == "--help" || a == "-h") {
        print_cli_help();
        return Ok(());
    }

    if args.first().map(String::as_str) == Some("update") {
        return update::run(update::Options {
            version: arg_value(&args, "--version"),
            mirror: arg_value(&args, "--mirror"),
        });
    }

    let mut cfg = config::Config::load()?;
    if args.iter().any(|a| a == "--init-config") {
        let path = config::Config::path();
        config::Config::save_default(&path)?;
        println!("wrote default config to {}", path.display());
        return Ok(());
    }

    // By default the TUI indexes the repository containing cwd. --wide restores
    // global discovery; an explicit --root always wins over it.
    if args.iter().any(|a| a == "--wide") {
        cfg.scan_roots = config::Config::global_roots();
        cfg.strict_roots = false;
    }
    let roots = arg_values(&args, "--root");
    if !roots.is_empty() {
        cfg.scan_roots = roots.into_iter().map(PathBuf::from).collect();
        cfg.strict_roots = true;
    }
    if args.iter().any(|a| a == "--all-modules") {
        cfg.launchable_only = false;
    }
    if let Some(d) = arg_value(&args, "--max-depth").and_then(|v| v.parse::<usize>().ok()) {
        cfg.max_depth = d;
    }
    if let Some(jdk) = arg_value(&args, "--jdk") {
        cfg.preferred_jdk = Some(jdk);
    }

    if args.iter().any(|a| a == "--scan-only" || a == "--dump") {
        return print_scan(&cfg);
    }
    if let Some(pattern) = arg_value(&args, "--launch") {
        return launch_cli(cfg, &args, &pattern);
    }

    install_panic_hook();
    enable_raw_mode()?;
    let mut stdout = std::io::stdout();
    execute!(stdout, EnterAlternateScreen, EnableMouseCapture)?;
    let backend = CrosstermBackend::new(stdout);
    let mut terminal = Terminal::new(backend)?;
    terminal.hide_cursor()?;

    // The scan is filesystem-bound; run it on a worker thread and keep drawing
    // a gauge so large trees or many toolchains never look like a hang.
    let inv = match scan_with_progress(&mut terminal, &cfg) {
        Ok(Some(inv)) => inv,
        Ok(None) => {
            restore_terminal(&mut terminal)?;
            println!("cancelled");
            return Ok(());
        }
        Err(e) => {
            restore_terminal(&mut terminal)?;
            return Err(e);
        }
    };

    let mut app = app::App::from_inventory(cfg, inv);
    let result = run_loop(&mut terminal, &mut app);

    // Always restore the terminal, even if the loop returned an error.
    restore_terminal(&mut terminal)?;
    result
}

fn restore_terminal(terminal: &mut Terminal<CrosstermBackend<Stdout>>) -> Result<()> {
    disable_raw_mode()?;
    execute!(
        terminal.backend_mut(),
        LeaveAlternateScreen,
        DisableMouseCapture
    )?;
    terminal.show_cursor()?;
    Ok(())
}

/// Run `scan::collect` on a worker thread while rendering [`progress::render`].
///
/// Returns `Ok(None)` when the user aborts with Esc / q / ctrl+c before the
/// scan finishes; the worker is then abandoned (the process exits right after).
fn scan_with_progress(
    terminal: &mut Terminal<CrosstermBackend<Stdout>>,
    cfg: &config::Config,
) -> Result<Option<model::Inventory>> {
    let shared = Arc::new(Mutex::new(progress::ScanProgress::new()));
    let (tx, rx) = mpsc::channel();
    {
        let cfg = cfg.clone();
        let shared = Arc::clone(&shared);
        std::thread::Builder::new()
            .name("tspb-scan".into())
            .spawn(move || {
                let rep = progress::Reporter::new(shared);
                let _ = tx.send(scan::collect_tracked(&cfg, &rep));
            })?;
    }

    // Fast scans finish before the first frame: skip the splash entirely.
    if let Ok(result) = rx.try_recv() {
        return result.map(Some);
    }

    loop {
        let snap = shared
            .lock()
            .map(|p| p.clone())
            .unwrap_or_else(|_| progress::ScanProgress::new());
        terminal.draw(|f| progress::render(f, &snap))?;

        match rx.try_recv() {
            Ok(result) => return result.map(Some),
            Err(TryRecvError::Empty) => {}
            Err(TryRecvError::Disconnected) => {
                bail!("scan worker stopped unexpectedly")
            }
        }

        if poll(Duration::from_millis(60))?
            && let crossterm::event::Event::Key(key) = read()?
        {
            use crossterm::event::{KeyCode, KeyModifiers};
            let ctrl_c = key.modifiers.contains(KeyModifiers::CONTROL)
                && matches!(key.code, KeyCode::Char('c'));
            if ctrl_c || matches!(key.code, KeyCode::Esc | KeyCode::Char('q')) {
                return Ok(None);
            }
        }
    }
}

fn arg_value(args: &[String], flag: &str) -> Option<String> {
    args.iter()
        .position(|a| a == flag)
        .and_then(|i| args.get(i + 1))
        .filter(|v| !v.starts_with("--"))
        .cloned()
}

fn arg_values(args: &[String], flag: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut it = args.iter().peekable();
    while let Some(a) = it.next() {
        if a == flag {
            while let Some(v) = it.peek() {
                if v.starts_with("--") {
                    break;
                }
                out.push(it.next().unwrap().clone());
            }
        }
    }
    out
}

/// Non-interactive launch used for smoke-testing a real project. Streams the same
/// log buffer the TUI shows, then stops the process tree (or detaches on request).
fn launch_cli(mut cfg: config::Config, args: &[String], pattern: &str) -> Result<()> {
    cfg.log_capacity = cfg.log_capacity.max(20_000);
    let inv = scan::collect(&cfg)?;
    let mut app = app::App::with_inventory(cfg, inv);
    let needle = pattern.to_lowercase();
    let mut hits: Vec<usize> = app
        .inv
        .projects
        .iter()
        .enumerate()
        .filter(|(_, p)| {
            p.name.to_lowercase().contains(&needle)
                || p.main_class
                    .as_deref()
                    .is_some_and(|m| m.to_lowercase().contains(&needle))
                || p.path.to_string_lossy().to_lowercase().contains(&needle)
        })
        .map(|(i, _)| i)
        .collect();
    if let Some(exact) = app
        .inv
        .projects
        .iter()
        .position(|p| p.name.eq_ignore_ascii_case(pattern))
    {
        hits = vec![exact];
    }
    if hits.is_empty() {
        bail!(
            "no Spring Boot project matches '{pattern}' ({} scanned)",
            app.inv.projects.len()
        );
    }
    if hits.len() > 1 {
        println!("ambiguous '{pattern}', pick one of:");
        for &i in &hits {
            let p = &app.inv.projects[i];
            println!(
                "  {}  main={:?}  {}",
                p.name,
                p.main_class,
                p.path.display()
            );
        }
        bail!("{} projects matched", hits.len());
    }

    let pi = hits[0];
    let p = app.inv.projects[pi].clone();
    println!(
        "project: {} | main={:?} | build={} | configs={}",
        p.name,
        p.main_class,
        p.build.label(),
        p.launch_configs
            .iter()
            .map(|c| c.name.clone())
            .collect::<Vec<_>>()
            .join(", ")
    );
    println!("cwd: {}", p.path.display());
    println!("jdk: {} | maven: {}", app.jdk_label(), app.maven_label());

    let id = app.launch_project(pi)?;
    let pid = app.runner.managed[&id].pid;
    println!("launched pid={pid} — streaming logs (Ctrl+C to abort early)\n");

    let watch = arg_value(args, "--watch-seconds")
        .and_then(|v| v.parse::<u64>().ok())
        .unwrap_or(90);
    let detach = args.iter().any(|a| a == "--detach");
    let started = Instant::now();
    let mut printed = 0usize;
    let mut verdict = "watch window elapsed";

    loop {
        app.runner.poll();
        let logs = app.runner.logs_for(&id);
        if logs.len() < printed {
            printed = 0; // ring buffer wrapped
        }
        for line in logs.iter().skip(printed) {
            // Colour codes stay in the buffer for the TUI; stdout stays plain.
            println!("{}", runner::plain_text(line));
        }
        printed = logs.len();
        let _ = std::io::stdout().flush();

        let state = app
            .runner
            .managed
            .get(&id)
            .map(|m| m.state)
            .unwrap_or(RunState::Exited);
        if matches!(state, RunState::Failed | RunState::Exited) {
            verdict = "process exited";
            break;
        }
        if started.elapsed() >= Duration::from_secs(watch) {
            break;
        }
        std::thread::sleep(Duration::from_millis(250));
    }

    println!(
        "\n-- {verdict} after {:.1}s --",
        started.elapsed().as_secs_f64()
    );
    if app
        .runner
        .managed
        .get(&id)
        .is_some_and(|m| m.state == RunState::Failed)
    {
        let logs = app.runner.logs_for(&id);
        let causes = [
            "Caused by:",
            "APPLICATION FAILED TO START",
            "CommunicationsException",
            "Connection refused",
            "BindException",
            "init datasource error",
            "Error creating bean",
            "UnknownHostException",
        ];
        let plain: Vec<String> = logs.iter().map(|l| runner::plain_text(l)).collect();
        let mut hints: Vec<String> = plain
            .iter()
            .filter(|l| causes.iter().any(|k| l.contains(k)) && !l.trim_start().starts_with("at "))
            .cloned()
            .collect();
        if hints.is_empty() {
            // Fall back to Maven's own errors, skipping boilerplate help lines.
            hints = plain
                .iter()
                .filter(|l| {
                    l.contains("ERROR")
                        && !l.contains("[Help ")
                        && !l.contains("To see the full stack trace")
                        && !l.contains("Re-run Maven")
                })
                .cloned()
                .collect();
        }
        let start = hints.len().saturating_sub(5);
        println!("diagnosis (last error lines):");
        for h in hints.drain(start..) {
            println!("  {}", h.trim());
        }
    }
    if detach {
        app.runner.detach_all();
        println!("-- detached: pid {pid} left running --");
    } else if app.runner.managed.contains_key(&id) {
        let _ = app.runner.stop(&id);
        println!("-- stopped pid {pid} (use --detach to keep it running) --");
    }
    Ok(())
}

fn run_loop(terminal: &mut Terminal<CrosstermBackend<Stdout>>, app: &mut app::App) -> Result<()> {
    loop {
        terminal.draw(|f| ui::render(f, app))?;
        let tick = Duration::from_millis(app.cfg.tick_ms.max(50));
        let deadline = Instant::now() + tick;
        loop {
            let remaining = deadline.saturating_duration_since(Instant::now());
            if !poll(remaining)? {
                break;
            }
            let ev = read()?;
            let action = event::translate(ev, app.prefix_active, app.is_editing(), app.prefix_char);
            if action == event::Action::Ignored {
                continue;
            }
            app.handle(action);
            break;
        }
        app.tick();
        if app.should_quit {
            break;
        }
    }
    // Detach: managed processes keep running after the client leaves.
    // Their reader threads still own the stdout/stderr pipes.
    app.runner.detach_all();
    Ok(())
}

fn install_panic_hook() {
    let original = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        let _ = disable_raw_mode();
        let _ = execute!(std::io::stdout(), LeaveAlternateScreen, DisableMouseCapture);
        original(info);
    }));
}

fn print_scan(cfg: &config::Config) -> Result<()> {
    let inv = scan::collect(cfg)?;
    println!("{}", inv.summary());
    println!(
        "scan roots: {}",
        cfg.effective_roots()
            .iter()
            .map(|p| p.display().to_string())
            .collect::<Vec<_>>()
            .join(", ")
    );

    println!("\n-- JDK / JRE --");
    for j in &inv.jdks {
        println!(
            "  {:30} {:14} [{:6}] {}  java={}",
            j.name,
            j.version,
            j.origin.label(),
            j.home.display(),
            j.java.display()
        );
    }

    println!("\n-- MAVEN --");
    for m in &inv.mavens {
        println!(
            "  {:30} {:14} [{:6}] {}  exe={}",
            m.name,
            m.version,
            m.origin.label(),
            m.home.display(),
            m.executable.display()
        );
    }

    println!("\n-- SPRING BOOT PROJECTS --");
    for p in &inv.projects {
        println!(
            "  {:30} {:8} [{}] {} ({} cfg, main={}, wrapper={})",
            p.name,
            p.build.label(),
            p.origin.label(),
            p.path.display(),
            p.launch_configs.len(),
            p.main_class.as_deref().unwrap_or("auto"),
            p.has_wrapper
        );
    }

    println!("\n-- RUNNING SPRING BOOT APPS --");
    for a in &inv.running {
        println!(
            "  {:30} pid={:<8} ports={:?} jdk={} wd={} managed={} [{}]",
            a.name,
            a.pid,
            a.ports,
            a.jdk_home
                .as_ref()
                .map(|p| p.display().to_string())
                .unwrap_or_default(),
            a.work_dir
                .as_ref()
                .map(|p| p.display().to_string())
                .unwrap_or_default(),
            a.managed,
            a.origin.label()
        );
        println!(
            "      main={}  cmd={}",
            a.main_class,
            a.cmdline.chars().take(160).collect::<String>()
        );
    }
    Ok(())
}

fn print_cli_help() {
    println!(
        "tspb - a terminal UI launcher for Spring Boot applications\n\
         \nUSAGE:\n  \
         tspb                            index cwd's project tree, then open the TUI\n  \
         tspb --scan-only                print discovered toolchains, launch items and processes\n  \
         tspb --launch NAME              start one project headlessly and stream its log\n  \
         tspb --wide                     index every configured/IDEA project tree\n  \
         tspb --init-config              write a default config.toml and exit\n  \
         tspb update                     replace this binary with the latest release\n  \
         tspb --help                     show this help\n\
         \nOPTIONS:\n  \
         --root DIR [DIR...]       index this tree instead of the current directory\n  \
         --wide                    index global roots plus IDEA recent projects\n  \
         --all-modules             also list non-launchable parent/library modules\n  \
         --max-depth N             project scan depth (default 6)\n  \
         --jdk SUBSTRING           force a JDK by name (overrides pom version matching)\n  \
         --watch-seconds N         log window for --launch (default 90)\n  \
         --detach                  keep the --launch process running on exit\n\
         \nUPDATE:\n  \
         tspb update               fetch the latest release and replace this binary\n  \
         tspb update --version TAG install a specific release (with or without the v)\n  \
         tspb update --mirror URL  download through a GitHub mirror prefix\n  \
         TSPB_MIRROR=URL           same as --mirror; without it, the direct URL is\n  \
                                   tried first and public mirrors are used as fallback\n\
         \nEXAMPLES:\n  \
         cd /path/to/your-spring-repo && tspb\n  \
         tspb --root /path/to/your-spring-repo --scan-only\n  \
         tspb --launch your-start-module --watch-seconds 180\n\
         \nIN THE TUI (Vim layers):\n  \
         PANES: h/j/k/l pick a window, Enter enters it, Esc clears filters\n  \
         CONTENT: j/k move, h/l navigate, Enter acts, Esc returns to PANES\n  \
         LOG: v select, y yank line, Y yank all, scrollbar on the right\n  \
         ctrl+b prefix still works; ctrl+b ? shows every binding\n\
         \nConfig: {}",
        config::Config::path().display()
    );
}
