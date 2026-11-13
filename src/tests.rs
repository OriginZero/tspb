use crate::app::{App, Focus, Layer, Mode, StatusFilter};
use crate::config::Config;
use crate::event::{self, Action};
use crate::model::*;
use crate::progress::{Reporter, STAGE_PROJECTS, ScanProgress};
use crate::runner::Runner;
use crossterm::event::{Event, KeyCode, KeyEvent, KeyEventKind, KeyModifiers};
use ratatui::Terminal;
use ratatui::backend::TestBackend;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};

fn key(code: KeyCode, mods: KeyModifiers) -> Event {
    Event::Key(KeyEvent::new(code, mods))
}

fn launch_config(name: &str, origin: &str) -> LaunchConfig {
    LaunchConfig {
        name: name.into(),
        main_class: Some("com.example.DemoApplication".into()),
        module_dir: None,
        vm_options: String::new(),
        program_args: String::new(),
        active_profiles: String::new(),
        origin: origin.into(),
    }
}

fn fake_inventory() -> Inventory {
    Inventory {
        jdks: vec![
            Jdk {
                name: "temurin-17".into(),
                home: PathBuf::from("/opt/jdk17"),
                version: "17.0.9".into(),
                java: PathBuf::from("/opt/jdk17/bin/java"),
                origin: Origin::Env,
            },
            Jdk {
                name: "IDEA JBR 21".into(),
                home: PathBuf::from("/opt/idea/jbr"),
                version: "21.0.1".into(),
                java: PathBuf::from("/opt/idea/jbr/bin/java"),
                origin: Origin::Idea,
            },
        ],
        mavens: vec![Maven {
            name: "IDEA Maven 3.9".into(),
            home: PathBuf::from("/opt/idea/plugins/maven3"),
            version: "3.9".into(),
            executable: PathBuf::from("/opt/idea/plugins/maven3/bin/mvn"),
            origin: Origin::Idea,
            wrapper: false,
        }],
        projects: vec![
            Project {
                name: "demo-app".into(),
                path: PathBuf::from("/work/demo-app"),
                build: BuildTool::Maven,
                has_wrapper: true,
                main_class: Some("com.example.DemoApplication".into()),
                java_version: Some(17),
                launch_configs: vec![
                    launch_config("DemoApplication", "IDEA run configuration"),
                    launch_config("spring-boot:run", "auto"),
                ],
                origin: Origin::Project,
            },
            Project {
                name: "gradle-app".into(),
                path: PathBuf::from("/work/gradle-app"),
                build: BuildTool::Gradle,
                has_wrapper: false,
                main_class: None,
                java_version: None,
                launch_configs: vec![launch_config("bootRun", "auto")],
                origin: Origin::Idea,
            },
        ],
        running: vec![RunningApp {
            pid: 4242,
            main_class: "com.example.DemoApplication".into(),
            name: "DemoApplication".into(),
            jdk_home: Some(PathBuf::from("/opt/jdk17")),
            work_dir: Some(PathBuf::from("/work/demo-app")),
            ports: vec![18080],
            cmdline: "java com.example.DemoApplication".into(),
            managed: false,
            origin: Origin::System,
        }],
    }
}

fn test_app() -> App {
    App::with_inventory(Config::default(), fake_inventory())
}

#[test]
fn prefix_reserves_exactly_one_key() {
    let kb = key(KeyCode::Char('b'), KeyModifiers::CONTROL);
    assert_eq!(event::translate(kb, false, false, 'b'), Action::Prefix);

    let q = key(KeyCode::Char('q'), KeyModifiers::NONE);
    assert_eq!(
        event::translate(q.clone(), true, false, 'b'),
        Action::Quit(false)
    );
    assert_eq!(event::translate(q, false, false, 'b'), Action::Quit(false));

    let ctrl_q = key(KeyCode::Char('Q'), KeyModifiers::CONTROL);
    assert_eq!(event::translate(ctrl_q, false, false, 'b'), Action::None);

    let prefix_help = key(KeyCode::Char('?'), KeyModifiers::NONE);
    assert_eq!(
        event::translate(prefix_help.clone(), true, false, 'b'),
        Action::Help
    );
    assert_eq!(
        event::translate(prefix_help, false, false, 'b'),
        Action::Help
    );

    assert_eq!(event::parse_prefix("ctrl+a"), 'a');
    assert_eq!(event::parse_prefix("^g"), 'g');
}

#[test]
fn key_up_and_auto_repeat_never_fire_a_second_action() {
    let keyup = |c: char, mods: KeyModifiers| {
        Event::Key(KeyEvent::new_with_kind(
            KeyCode::Char(c),
            mods,
            KeyEventKind::Release,
        ))
    };
    let repeat = |c: char, mods: KeyModifiers| {
        Event::Key(KeyEvent::new_with_kind(
            KeyCode::Char(c),
            mods,
            KeyEventKind::Repeat,
        ))
    };

    assert_eq!(
        event::translate(keyup('j', KeyModifiers::NONE), false, false, 'b'),
        Action::Ignored
    );
    assert_eq!(
        event::translate(repeat('j', KeyModifiers::NONE), false, false, 'b'),
        Action::Ignored
    );

    // Win32 sends Press + Release (and enhanced terminals add Repeat): the full
    // sequence for one tap must move the cursor exactly once.
    let mut app = test_app();
    assert_eq!(app.layer, Layer::Panes);
    assert!(app.hover.is_none(), "startup must not select any window");
    // Enter ITEMS first: in PANES a `j` only picks a window, it does not move rows.
    app.handle(Action::Enter);
    assert_eq!(app.layer, Layer::Content);
    let start = app.selected;
    app.handle(event::translate(
        key(KeyCode::Char('j'), KeyModifiers::NONE),
        false,
        false,
        'b',
    ));
    app.handle(event::translate(
        keyup('j', KeyModifiers::NONE),
        false,
        false,
        'b',
    ));
    app.handle(event::translate(
        repeat('j', KeyModifiers::NONE),
        false,
        false,
        'b',
    ));
    assert_eq!(
        app.selected,
        start + 1,
        "one physical keystroke must move exactly once"
    );

    // Releasing ctrl+b must not consume the prefix it just opened.
    app.handle(event::translate(
        key(KeyCode::Char('b'), KeyModifiers::CONTROL),
        false,
        false,
        'b',
    ));
    assert!(app.prefix_active);
    app.handle(event::translate(
        keyup('b', KeyModifiers::CONTROL),
        true,
        false,
        'b',
    ));
    assert!(
        app.prefix_active,
        "key-up must not eat the pending prefix chord"
    );
    app.handle(event::translate(
        key(KeyCode::Char('?'), KeyModifiers::NONE),
        true,
        false,
        'b',
    ));
    assert_eq!(
        app.mode,
        Mode::Help,
        "the next real press still uses prefix"
    );

    // Filter editing must not receive a duplicate character either.
    let mut edit = test_app();
    edit.mode = Mode::Filter;
    edit.filter.clear();
    edit.handle(event::translate(
        key(KeyCode::Char('s'), KeyModifiers::NONE),
        false,
        true,
        'b',
    ));
    edit.handle(event::translate(
        keyup('s', KeyModifiers::NONE),
        false,
        true,
        'b',
    ));
    assert_eq!(edit.filter, "s", "key-up must not type a second character");
}

#[test]
fn direct_keys_and_status_filters() {
    assert_eq!(
        event::translate(
            key(KeyCode::Char('j'), KeyModifiers::NONE),
            false,
            false,
            'b'
        ),
        Action::Vim('j')
    );
    assert_eq!(
        event::translate(
            key(KeyCode::Char('k'), KeyModifiers::NONE),
            false,
            false,
            'b'
        ),
        Action::Vim('k')
    );
    assert_eq!(
        event::translate(key(KeyCode::Down, KeyModifiers::NONE), false, false, 'b'),
        Action::Vim('j'),
        "arrow keys share the Vim layer semantics"
    );
    assert_eq!(
        event::translate(key(KeyCode::Left, KeyModifiers::NONE), false, false, 'b'),
        Action::Vim('h')
    );
    for (c, expected) in [
        ('w', StatusFilter::Working),
        ('b', StatusFilter::Blocked),
        ('i', StatusFilter::Idle),
        ('d', StatusFilter::Done),
    ] {
        assert_eq!(
            event::translate(key(KeyCode::Char(c), KeyModifiers::NONE), false, false, 'b'),
            Action::StatusFilter(c)
        );
        assert_eq!(StatusFilter::from_key(c), Some(expected));
    }
    assert!(StatusFilter::Done.matches(RunState::Running));
    assert!(StatusFilter::Working.matches(RunState::Starting));
    assert!(StatusFilter::Blocked.matches(RunState::Failed));
    assert!(StatusFilter::Idle.matches(RunState::Exited));
}

#[test]
fn tui_renders_every_mode_and_layout_without_panic() {
    let mut app = test_app();
    let mut terminal = Terminal::new(TestBackend::new(140, 44)).unwrap();
    for mode in [
        Mode::Normal,
        Mode::Filter,
        Mode::Help,
        Mode::Goto,
        Mode::JdkPicker,
        Mode::MavenPicker,
        Mode::ConfigPicker,
        Mode::ConfirmStop,
        Mode::ConfirmQuit,
    ] {
        app.mode = mode;
        terminal.draw(|f| crate::ui::render(f, &mut app)).unwrap();
    }
    app.mode = Mode::Normal;
    for (zoom, sidebar, log) in [
        (Some(Focus::Log), false, true),
        (Some(Focus::Sidebar), true, false),
        (Some(Focus::List), false, false),
        (None, true, true),
    ] {
        app.zoom = zoom;
        app.show_sidebar = sidebar;
        app.show_log = log;
        terminal.draw(|f| crate::ui::render(f, &mut app)).unwrap();
    }
    // Tiny terminal must not underflow any layout calculation.
    let mut small = Terminal::new(TestBackend::new(30, 10)).unwrap();
    small.draw(|f| crate::ui::render(f, &mut app)).unwrap();
}

#[test]
fn state_machine_navigation_filters_and_pickers() {
    let mut app = test_app();
    assert!(!app.rows.is_empty());
    app.handle(Action::Enter); // PANES -> CONTENT/ITEMS

    let start = app.selected;
    assert_eq!(
        app.rows[start].group, "PROJECTS",
        "cursor should open on the first launchable item"
    );
    app.handle(Action::Move(1));
    assert_eq!(app.selected, start + 1);
    app.handle(Action::Home);
    assert_eq!(app.selected, 0);

    app.handle(Action::View(2));
    assert_eq!(app.view, View::Projects);
    assert!(app.rows.iter().all(|r| r.group == "PROJECTS"));

    app.handle(Action::View(0));
    app.handle(Action::StatusFilter('d'));
    assert_eq!(app.status_filter, Some(StatusFilter::Done));
    assert!(!app.rows.is_empty());
    assert!(app.rows.iter().all(|r| r.state == RunState::Running));
    app.handle(Action::Esc); // CONTENT -> PANES
    assert_eq!(app.layer, Layer::Panes);
    app.handle(Action::Esc); // PANES Esc clears filters
    assert_eq!(app.status_filter, None);

    app.handle(Action::JdkPicker);
    assert_eq!(app.mode, Mode::JdkPicker);
    app.handle(Action::Move(1));
    app.handle(Action::Enter);
    assert_eq!(app.mode, Mode::Normal);
    assert_eq!(app.active_jdk, 1);

    app.handle(Action::MavenPicker);
    app.handle(Action::Enter);
    assert_eq!(app.active_maven, 0);

    app.handle(Action::Help);
    assert_eq!(app.mode, Mode::Help);
    app.handle(Action::Esc);
    assert_eq!(app.mode, Mode::Normal);

    app.handle(Action::Goto);
    assert_eq!(app.mode, Mode::Goto);
    app.handle(Action::FilterChar('d'));
    app.handle(Action::Enter);
    assert_eq!(app.mode, Mode::Normal);

    app.handle(Action::Quit(false));
    assert!(app.should_quit);
}

#[test]
fn confirm_gate_for_external_process_stop() {
    let mut app = test_app();
    // First row is the externally-started app in the RUNNING group.
    let running_idx = app.rows.iter().position(|r| r.group == "RUNNING").unwrap();
    app.selected = running_idx;
    app.handle(Action::Stop);
    assert_eq!(
        app.mode,
        Mode::ConfirmStop,
        "stopping an external JVM must ask first"
    );
    app.handle(Action::Esc);
    assert_eq!(app.mode, Mode::Normal);
    assert!(app.confirm_pid.is_none());
}

#[test]
fn config_roundtrip_preserves_settings() {
    let cfg = Config {
        prefix: "ctrl+a".into(),
        preferred_jdk: Some("temurin-21".into()),
        extra_args: vec!["--spring.profiles.active=dev".into()],
        skip_jbr: true,
        ..Default::default()
    };

    let raw = toml::to_string_pretty(&cfg).unwrap();
    let parsed: Config = toml::from_str(&raw).unwrap();
    assert_eq!(parsed.prefix, "ctrl+a");
    assert_eq!(parsed.preferred_jdk.as_deref(), Some("temurin-21"));
    assert_eq!(parsed.extra_args, cfg.extra_args);
    assert!(parsed.skip_jbr);
    assert_eq!(parsed.max_depth, cfg.max_depth);
}

#[test]
fn real_inventory_discovery_does_not_panic() {
    let inv = crate::scan::collect(&Config::default()).unwrap();
    println!("discovered: {}", inv.summary());
    assert!(inv.summary().contains("JDK"));
}

#[cfg(windows)]
#[test]
fn runner_streams_logs_and_kills_the_process_tree() {
    use std::fs;
    use std::thread::sleep;
    use std::time::Duration;

    let tmp = std::env::temp_dir().join(format!("tspb-runner-{}", std::process::id()));
    let _ = fs::remove_dir_all(&tmp);
    let bin = tmp.join("bin");
    fs::create_dir_all(&bin).unwrap();
    // A fake mvn.cmd that prints a marker and then stays alive long enough to be killed.
    fs::write(
        bin.join("mvn.cmd"),
        "@echo off\r\necho TSPB_MARKER_OK\r\nping -n 30 127.0.0.1 >nul\r\n",
    )
    .unwrap();

    let jdk = Jdk {
        name: "test-jdk".into(),
        home: tmp.clone(),
        version: "17".into(),
        java: bin.join("java.exe"),
        origin: Origin::Env,
    };
    let mvn = Maven {
        name: "fake-maven".into(),
        home: tmp.clone(),
        version: "3".into(),
        executable: bin.join("mvn.cmd"),
        origin: Origin::Env,
        wrapper: false,
    };
    let project = Project {
        name: "fake-app".into(),
        path: tmp.clone(),
        build: BuildTool::Maven,
        has_wrapper: false,
        main_class: None,
        java_version: None,
        launch_configs: vec![launch_config("spring-boot:run", "auto")],
        origin: Origin::Project,
    };
    let cfg = Config::default();
    let launch = project.launch_configs[0].clone();
    let mut runner = Runner::new(100);
    let id = runner
        .launch(&project, &launch, &jdk, Some(&mvn), &cfg)
        .unwrap();

    let mut logs = Vec::new();
    for _ in 0..20 {
        sleep(Duration::from_millis(150));
        runner.poll();
        logs = runner.logs_for(&id);
        if logs.iter().any(|l| l.contains("TSPB_MARKER_OK")) {
            break;
        }
    }
    assert!(
        logs.iter().any(|l| l.contains("TSPB_MARKER_OK")),
        "stdout was not captured: {logs:?}"
    );
    assert!(
        logs.iter().any(|l| l.contains("spring-boot:run")),
        "command line missing: {logs:?}"
    );
    assert!(runner.find_by_pid(runner.managed[&id].pid).is_some());

    runner.stop(&id).unwrap();
    assert!(runner.managed.is_empty());
    let _ = fs::remove_dir_all(&tmp);
}

#[test]
fn jdk_selection_honours_the_project_java_version() {
    let mut inv = fake_inventory();
    inv.jdks.insert(
        0,
        Jdk {
            name: "IDEA JBR 25".into(),
            home: PathBuf::from("/opt/idea/jbr25"),
            version: "25.0.4".into(),
            java: PathBuf::from("/opt/idea/jbr25/bin/java"),
            origin: Origin::Idea,
        },
    );
    let app = App::with_inventory(Config::default(), inv);
    assert!(
        app.jdk_label().contains("17"),
        "a Java 17 project must not default to JBR 25, got {}",
        app.jdk_label()
    );

    let explicit = Config {
        preferred_jdk: Some("jbr 21".into()),
        ..Default::default()
    };
    let app = App::with_inventory(explicit, fake_inventory());
    assert!(
        app.jdk_label().contains("JBR 21"),
        "explicit --jdk must win"
    );
}

#[test]
fn cli_argument_parsing_handles_repeated_roots() {
    let args: Vec<String> = [
        "--root",
        "a",
        "b",
        "--max-depth",
        "4",
        "--launch",
        "demo",
        "--detach",
    ]
    .iter()
    .map(|s| s.to_string())
    .collect();
    assert_eq!(crate::arg_values(&args, "--root"), vec!["a", "b"]);
    assert_eq!(crate::arg_value(&args, "--max-depth").as_deref(), Some("4"));
    assert_eq!(crate::arg_value(&args, "--launch").as_deref(), Some("demo"));
    assert!(crate::arg_value(&args, "--detach").is_none());
}

#[test]
fn multi_module_idea_run_configuration_is_bound_to_its_module() {
    use std::fs;

    let tmp = std::env::temp_dir().join(format!("tspb-multimodule-{}", std::process::id()));
    let _ = fs::remove_dir_all(&tmp);
    let module = tmp.join("server").join("app-start");
    fs::create_dir_all(module.join("src/main/java/com/example")).unwrap();
    fs::create_dir_all(tmp.join(".idea")).unwrap();
    fs::write(
        tmp.join("pom.xml"),
        "<project><artifactId>parent</artifactId><modules><module>server</module></modules>\
         <properties><java.version>17</java.version></properties></project>",
    )
    .unwrap();
    fs::write(
        module.join("pom.xml"),
        "<project><artifactId>app-start</artifactId><build><plugins><plugin>\
         <groupId>org.springframework.boot</groupId>\
         <artifactId>spring-boot-maven-plugin</artifactId></plugin></plugins></build></project>",
    )
    .unwrap();
    fs::write(
        module.join("src/main/java/com/example/DemoApplication.java"),
        "package com.example;\n@SpringBootApplication\npublic class DemoApplication {}\n",
    )
    .unwrap();
    fs::write(
        tmp.join(".idea/workspace.xml"),
        r#"<?xml version="1.0"?><project><component name="RunManager">
  <configuration name="DemoApplication" type="Application" temporary="true">
    <option name="MAIN_CLASS_NAME" value="com.example.DemoApplication" />
    <option name="VM_PARAMETERS" value="-Xmx1g" />
    <option name="PROGRAM_PARAMETERS" value="--spring.profiles.active=dev" />
  </configuration>
  <configuration default="true" type="Application">
    <option name="MAIN_CLASS_NAME" value="ignored" />
  </configuration>
  <configuration name="Unrelated" type="Application">
    <option name="MAIN_CLASS_NAME" value="com.other.Other" />
  </configuration>
</component></project>"#,
    )
    .unwrap();

    let cfg = Config {
        scan_roots: vec![tmp.clone()],
        max_depth: 3,
        strict_roots: true,
        ..Default::default()
    };
    let projects = crate::scan::project::scan(&cfg, &[], &crate::progress::Reporter::silent());
    let p = projects
        .iter()
        .find(|p| p.name == "app-start")
        .expect("module not discovered");
    assert_eq!(p.main_class.as_deref(), Some("com.example.DemoApplication"));
    assert_eq!(
        p.java_version,
        Some(17),
        "root pom java.version not inherited"
    );
    assert!(
        p.launch_configs.iter().any(|c| c.name == "DemoApplication"
            && c.vm_options == "-Xmx1g"
            && c.program_args.contains("dev")),
        "root .idea configuration was not bound: {:?}",
        p.launch_configs
            .iter()
            .map(|c| c.name.clone())
            .collect::<Vec<_>>()
    );
    assert!(
        !p.launch_configs.iter().any(|c| c.name == "Unrelated"),
        "unrelated IDEA config must not leak into the module"
    );
    assert!(p.launch_configs.iter().any(|c| c.name == "spring-boot:run"));
    let _ = fs::remove_dir_all(&tmp);
}

#[test]
fn example_config_matches_current_schema() {
    let raw = std::fs::read_to_string("config.example.toml").unwrap();
    let cfg: Config = toml::from_str(&raw).unwrap();
    assert!(cfg.scan_roots.is_empty());
    assert!(cfg.strict_roots && cfg.launchable_only);
}

#[test]
fn default_config_indexes_only_the_current_tree() {
    let cfg = Config::default();
    assert!(
        cfg.scan_roots.is_empty(),
        "empty roots means cwd auto-detection"
    );
    assert!(
        cfg.strict_roots,
        "IDEA history must not leak into a cwd scan"
    );
    assert!(cfg.launchable_only);
    assert_eq!(cfg.effective_roots().len(), 1);
}

#[test]
fn cwd_inside_a_module_resolves_to_the_checkout_root() {
    use std::fs;

    let tmp = std::env::temp_dir().join(format!("tspb-locate-{}", std::process::id()));
    let _ = fs::remove_dir_all(&tmp);
    let module = tmp.join("modules").join("app-start");
    fs::create_dir_all(&module).unwrap();
    fs::create_dir_all(tmp.join(".git")).unwrap();
    fs::write(
        tmp.join("pom.xml"),
        "<project><modules><module>modules</module></modules></project>",
    )
    .unwrap();
    fs::write(module.join("pom.xml"), "<project/>").unwrap();

    assert_eq!(crate::scan::project::locate_repo_root(&module), tmp);
    let _ = fs::remove_dir_all(&tmp);
}

#[test]
fn launchable_filter_keeps_only_runnable_modules() {
    use std::fs;

    let tmp = std::env::temp_dir().join(format!("tspb-launchable-{}", std::process::id()));
    let _ = fs::remove_dir_all(&tmp);
    let repo = tmp.join("repo");
    let app = repo.join("app-start");
    let lib = repo.join("core");
    fs::create_dir_all(app.join("src/main/java/demo")).unwrap();
    fs::create_dir_all(lib.join("src/main/java/demo")).unwrap();
    fs::write(
        repo.join("pom.xml"),
        "<project><modules><module>app-start</module><module>core</module></modules></project>",
    )
    .unwrap();
    for module in [&app, &lib] {
        fs::write(
            module.join("pom.xml"),
            "<project><groupId>org.springframework.boot</groupId></project>",
        )
        .unwrap();
    }
    fs::write(
        lib.join("src/main/java/demo/Helper.java"),
        "package demo;\npublic class Helper {}\n",
    )
    .unwrap();
    fs::write(
        app.join("src/main/java/demo/DemoApplication.java"),
        "package demo;\n@SpringBootApplication\npublic class DemoApplication {}\n",
    )
    .unwrap();

    let cfg = Config {
        scan_roots: vec![repo.clone()],
        max_depth: 3,
        strict_roots: true,
        ..Default::default()
    };
    let names: Vec<String> =
        crate::scan::project::scan(&cfg, &[], &crate::progress::Reporter::silent())
            .iter()
            .map(|p| p.name.clone())
            .collect();
    assert_eq!(
        names,
        vec!["app-start".to_string()],
        "only the runnable module should be listed: {names:?}"
    );

    let loose = Config {
        launchable_only: false,
        ..cfg.clone()
    };
    let all = crate::scan::project::scan(&loose, &[], &crate::progress::Reporter::silent());
    assert!(
        all.iter().any(|p| p.name == "core"),
        "--all-modules must expose library modules"
    );

    // With no main class anywhere, fall back to every Spring module rather than
    // opening an empty picker.
    fs::remove_file(app.join("src/main/java/demo/DemoApplication.java")).unwrap();
    let fallback = crate::scan::project::scan(&cfg, &[], &crate::progress::Reporter::silent());
    assert_eq!(fallback.len(), 2, "fallback must not be empty");
    let _ = fs::remove_dir_all(&tmp);
}

#[test]
fn vim_layers_start_with_nothing_selected() {
    let mut app = test_app();
    assert_eq!(app.layer, Layer::Panes);
    assert!(app.hover.is_none(), "no window is selected on startup");
    let start = app.selected;

    app.handle(Action::Vim('h'));
    assert_eq!(app.hover, Some(Focus::Sidebar));
    assert_eq!(app.layer, Layer::Panes);
    assert_eq!(app.selected, start, "PANES navigation must not move rows");

    app.handle(Action::Vim('j'));
    assert_eq!(app.hover, Some(Focus::Log));
    app.handle(Action::Vim('k'));
    assert_eq!(app.hover, Some(Focus::List));

    app.handle(Action::Enter);
    assert_eq!(app.layer, Layer::Content);
    assert_eq!(app.focus, Focus::List);
    app.handle(Action::Vim('j'));
    assert_eq!(app.selected, start + 1);

    let view = app.view;
    app.handle(Action::Vim('h'));
    assert!(app.view != view, "h/l switch workspace inside ITEMS");

    app.handle(Action::Esc);
    assert_eq!(app.layer, Layer::Panes);
    let held = app.selected;
    app.handle(Action::Vim('j'));
    assert_eq!(app.selected, held, "back in PANES, j only picks a window");
    assert_eq!(app.hover, Some(Focus::Log));
}

#[test]
fn log_yank_ranges_cover_line_visual_and_all() {
    let lines: Vec<String> = (1..=5).map(|i| format!("line-{i}")).collect();
    let mut app = test_app();

    app.log_cursor = 2;
    assert_eq!(app.yank_text(&lines, false), "line-3");

    app.visual_anchor = Some(0);
    assert_eq!(app.yank_text(&lines, false), "line-1\nline-2\nline-3");
    app.log_cursor = 1;
    assert_eq!(app.yank_text(&lines, false), "line-1\nline-2");

    assert_eq!(
        app.yank_text(&lines, true),
        "line-1\nline-2\nline-3\nline-4\nline-5"
    );

    app.visual_anchor = None;
    app.log_cursor = 99;
    assert_eq!(app.yank_text(&lines, false), "line-5", "cursor is clamped");
    assert!(app.yank_text(&[], true).is_empty());
}

#[test]
fn renders_panes_content_log_cursor_and_visual_without_panic() {
    let mut app = test_app();
    let mut terminal = Terminal::new(TestBackend::new(140, 44)).unwrap();
    for (layer, focus, visual) in [
        (Layer::Panes, Focus::List, false),
        (Layer::Content, Focus::List, false),
        (Layer::Content, Focus::Sidebar, false),
        (Layer::Content, Focus::Log, false),
        (Layer::Content, Focus::Log, true),
    ] {
        app.layer = layer;
        app.focus = focus;
        app.hover = if layer == Layer::Panes {
            None
        } else {
            Some(focus)
        };
        app.visual_anchor = visual.then_some(0);
        app.log_cursor = 0;
        terminal.draw(|f| crate::ui::render(f, &mut app)).unwrap();
    }
    let mut small = Terminal::new(TestBackend::new(28, 8)).unwrap();
    small.draw(|f| crate::ui::render(f, &mut app)).unwrap();
}

/// A real short-lived process so `current_log_id()` / `logs_for()` are exercised
/// through the same path as a live `spring-boot:run`.
#[cfg(windows)]
fn fake_log_runner() -> (Runner, String, PathBuf) {
    use std::fs;
    use std::thread::sleep;
    use std::time::Duration;

    let tmp = std::env::temp_dir().join(format!("tspb-yank-{}", std::process::id()));
    let _ = fs::remove_dir_all(&tmp);
    let bin = tmp.join("bin");
    fs::create_dir_all(&bin).unwrap();
    fs::write(
        bin.join("mvn.cmd"),
        "@echo off\r\necho YANK_LINE_ONE\r\necho YANK_LINE_TWO\r\nping -n 30 127.0.0.1 >nul\r\n",
    )
    .unwrap();
    let jdk = Jdk {
        name: "test-jdk".into(),
        home: tmp.clone(),
        version: "17".into(),
        java: bin.join("java.exe"),
        origin: Origin::Env,
    };
    let mvn = Maven {
        name: "fake-maven".into(),
        home: tmp.clone(),
        version: "3".into(),
        executable: bin.join("mvn.cmd"),
        origin: Origin::Env,
        wrapper: false,
    };
    let project = Project {
        name: "fake-app".into(),
        path: tmp.clone(),
        build: BuildTool::Maven,
        has_wrapper: false,
        main_class: None,
        java_version: None,
        launch_configs: vec![launch_config("spring-boot:run", "auto")],
        origin: Origin::Project,
    };
    let launch = project.launch_configs[0].clone();
    let mut runner = Runner::new(100);
    let id = runner
        .launch(&project, &launch, &jdk, Some(&mvn), &Config::default())
        .unwrap();
    for _ in 0..20 {
        sleep(Duration::from_millis(150));
        runner.poll();
        if runner
            .logs_for(&id)
            .iter()
            .any(|l| l.contains("YANK_LINE_TWO"))
        {
            break;
        }
    }
    (runner, id, tmp)
}

#[cfg(windows)]
#[test]
fn log_window_yanks_line_range_and_whole_log() {
    let (runner, id, tmp) = fake_log_runner();
    let mut app = test_app();
    app.runner = runner;
    app.selected_log_id = Some(id.clone());
    let total = app.runner.log_len(&id);
    assert!(
        total >= 3,
        "expected command line + two yank lines: {total}"
    );

    app.handle(Action::Vim('j')); // PANES -> LOG
    app.handle(Action::Enter); // CONTENT/LOG
    assert!(app.is_log_content());
    assert_eq!(app.log_cursor, total - 1);

    // A real log buffer also exercises the scrollbar + cursor-highlight branch.
    let mut terminal = Terminal::new(TestBackend::new(120, 40)).unwrap();
    terminal.draw(|f| crate::ui::render(f, &mut app)).unwrap();

    app.handle(Action::CopyLine); // y
    assert_eq!(app.clipboard.as_deref().unwrap().lines().count(), 1);

    app.handle(Action::Vim('k'));
    app.handle(Action::Visual); // v
    app.handle(Action::Vim('k'));
    app.handle(Action::CopyLine); // y over VISUAL range
    assert_eq!(app.clipboard.as_deref().unwrap().lines().count(), 2);
    assert!(app.visual_anchor.is_none(), "a yank ends VISUAL mode");

    app.handle(Action::CopyAll); // Y
    assert_eq!(app.clipboard.as_deref().unwrap().lines().count(), total);

    let _ = app.runner.stop(&id);
    let _ = std::fs::remove_dir_all(&tmp);
}

fn tracked() -> (Arc<Mutex<ScanProgress>>, Reporter) {
    let shared = Arc::new(Mutex::new(ScanProgress::new()));
    let rep = Reporter::new(Arc::clone(&shared));
    (shared, rep)
}

#[test]
fn startup_progress_advances_with_stages_and_dirs() {
    let (shared, rep) = tracked();
    assert!(shared.lock().unwrap().ratio() < 0.1);

    rep.stage(STAGE_PROJECTS);
    rep.dirs(400);
    rep.projects(3);
    let ratio = shared.lock().unwrap().ratio();
    // Two stages done plus half of the project walk.
    assert!((0.6..0.7).contains(&ratio), "ratio after 400 dirs: {ratio}");

    rep.dirs(4000);
    assert!(shared.lock().unwrap().ratio() > ratio);

    rep.finish();
    assert_eq!(shared.lock().unwrap().ratio(), 1.0);

    // The silent reporter used by --scan-only, rescans and tests is a no-op.
    let silent = Reporter::silent();
    silent.stage(STAGE_PROJECTS);
    silent.dirs(1);
    silent.projects(1);
    silent.finish();
}

#[test]
fn startup_progress_renders_stage_counters_and_clips_long_paths() {
    let (shared, rep) = tracked();
    rep.stage(STAGE_PROJECTS);
    rep.dirs(1200);
    rep.projects(7);
    rep.jdks(3);
    rep.mavens(2);
    rep.detail(
        "C:\\Users\\someone\\Desktop\\Project\\a-really-long-repository-path-that-needs-clipping",
    );
    let snap = shared.lock().unwrap().clone();

    let mut terminal = Terminal::new(TestBackend::new(90, 24)).unwrap();
    terminal
        .draw(|f| crate::progress::render(f, &snap))
        .unwrap();
    let text = terminal
        .backend()
        .buffer()
        .content()
        .iter()
        .map(|c| c.symbol())
        .collect::<String>();
    assert!(text.contains("indexing workspace"));
    assert!(text.contains("Indexing Spring Boot projects"));
    assert!(text.contains("3 / 4"));
    assert!(text.contains("projects 7"));
    assert!(text.contains("dirs 1200"));
    assert!(text.contains('…'), "long root paths are ellipsized");

    // Both fallback layouts (too small for the box) must not panic either.
    for (w, h) in [(30u16, 6u16), (20, 4), (10, 2)] {
        let mut tiny = Terminal::new(TestBackend::new(w, h)).unwrap();
        tiny.draw(|f| crate::progress::render(f, &snap)).unwrap();
    }
}

/// Start a fake `spring-boot:run` shim that prints `{tag}_READY`, then stays
/// alive for ~30s. Two of them give the TUI two independent streams to switch
/// between, exercising `current_log_id()` the way two real services would.
#[cfg(windows)]
fn fake_service(dir: &str, tag: &str, jdk: &Jdk, runner: &mut Runner) -> (Project, String) {
    use std::fs;
    let tmp = std::env::temp_dir().join(format!("tspb-svc-{}-{dir}", std::process::id()));
    let _ = fs::remove_dir_all(&tmp);
    let bin = tmp.join("bin");
    fs::create_dir_all(&bin).unwrap();
    fs::write(
        bin.join("mvn.cmd"),
        format!(
            "@echo off\r\necho {tag}_READY\r\necho {tag}_SECOND\r\nping -n 30 127.0.0.1 >nul\r\n"
        ),
    )
    .unwrap();
    let mvn = Maven {
        name: format!("mvn-{dir}"),
        home: tmp.clone(),
        version: "3".into(),
        executable: bin.join("mvn.cmd"),
        origin: Origin::Env,
        wrapper: false,
    };
    let project = Project {
        name: dir.into(),
        path: tmp,
        build: BuildTool::Maven,
        has_wrapper: false,
        main_class: None,
        java_version: None,
        launch_configs: vec![launch_config("auto", "auto")],
        origin: Origin::Project,
    };
    let id = runner
        .launch(
            &project,
            &project.launch_configs[0],
            jdk,
            Some(&mvn),
            &Config::default(),
        )
        .unwrap();
    (project, id)
}

#[cfg(windows)]
fn wait_for_line(runner: &mut Runner, id: &str, needle: &str) {
    for _ in 0..40 {
        std::thread::sleep(std::time::Duration::from_millis(100));
        runner.poll();
        if runner.logs_for(id).iter().any(|l| l.contains(needle)) {
            return;
        }
    }
    panic!("log line {needle} never appeared for {id}");
}

#[cfg(windows)]
#[test]
fn selecting_another_running_row_switches_detail_and_log() {
    let jdk = Jdk {
        name: "test-jdk".into(),
        home: std::env::temp_dir(),
        version: "17".into(),
        java: PathBuf::from("java.exe"),
        origin: Origin::Env,
    };
    let mut runner = Runner::new(200);
    let (proj_a, id_a) = fake_service("svc-a", "ALPHA", &jdk, &mut runner);
    let (proj_b, id_b) = fake_service("svc-b", "BETA", &jdk, &mut runner);
    wait_for_line(&mut runner, &id_a, "ALPHA_READY");
    wait_for_line(&mut runner, &id_b, "BETA_READY");
    let pid_a = runner.managed[&id_a].pid;
    let pid_b = runner.managed[&id_b].pid;

    let mut app = test_app();
    app.inv.projects = vec![proj_a, proj_b];
    app.runner = runner;
    app.rebuild_rows();

    let row_of = |app: &App, name: &str| {
        app.rows
            .iter()
            .position(|r| {
                matches!(r.kind, RowKind::App { pid: Some(_), .. }) && r.label.starts_with(name)
            })
            .unwrap_or_else(|| panic!("no running row for {name}"))
    };
    let (row_a, row_b) = (row_of(&app, "svc-a"), row_of(&app, "svc-b"));

    // Cursor on A: log, DETAIL and the live pid must all follow A.
    app.selected = row_a;
    app.tick();
    assert_eq!(app.current_log_id().as_deref(), Some(id_a.as_str()));
    assert_eq!(app.log_cursor, app.runner.log_len(&id_a).saturating_sub(1));

    // Cursor on B: the pane switches without pressing Enter on it first.
    app.selected = row_b;
    app.tick();
    assert_eq!(app.current_log_id().as_deref(), Some(id_b.as_str()));
    assert_eq!(app.selected_log_id.as_deref(), Some(id_b.as_str()));
    assert!(
        app.log_follow,
        "switching rows re-attaches to the live tail"
    );
    assert_eq!(app.log_cursor, app.runner.log_len(&id_b).saturating_sub(1));

    let mut terminal = Terminal::new(TestBackend::new(120, 40)).unwrap();
    terminal.draw(|f| crate::ui::render(f, &mut app)).unwrap();
    let screen: String = terminal
        .backend()
        .buffer()
        .content()
        .iter()
        .map(|c| c.symbol())
        .collect();
    assert!(
        screen.contains("BETA_READY"),
        "LOG pane should render the selected stream"
    );
    assert!(
        !screen.contains("ALPHA_READY"),
        "the other service's stream must not leak into the pane"
    );
    // The list shows every pid, so check the DETAIL/LIVE LOG line specifically.
    let detail_row = screen
        .chars()
        .collect::<Vec<_>>()
        .chunks(120)
        .map(|c| c.iter().collect::<String>())
        .find(|r| r.contains("LIVE LOG"))
        .expect("DETAIL renders a LIVE LOG line");
    assert!(
        detail_row.contains(&format!("pid {pid_b}")),
        "DETAIL must report the pid of the selected row: {detail_row}"
    );
    assert!(
        !detail_row.contains(&format!("pid {pid_a}")),
        "DETAIL still points at the previous row: {detail_row}"
    );

    // Going back works the same way, and toolchain rows keep the last stream.
    app.selected = row_a;
    app.tick();
    assert_eq!(app.current_log_id().as_deref(), Some(id_a.as_str()));
    let jdk_row = app
        .rows
        .iter()
        .position(|r| matches!(r.kind, RowKind::Jdk(_)))
        .expect("inventory has a JDK row");
    app.selected = jdk_row;
    app.tick();
    assert_eq!(
        app.current_log_id().as_deref(),
        Some(id_a.as_str()),
        "a non-app row keeps the last stream instead of blanking the pane"
    );

    let _ = app.runner.stop(&id_a);
    let _ = app.runner.stop(&id_b);
    for dir in ["svc-a", "svc-b"] {
        let _ = std::fs::remove_dir_all(
            std::env::temp_dir().join(format!("tspb-svc-{}-{dir}", std::process::id())),
        );
    }
}

