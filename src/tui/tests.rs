use std::path::{Path, PathBuf};

use ratatui::Terminal;
use ratatui::backend::TestBackend;
use ratatui::crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

use super::model::{App, Command, Kind, Location, Mark, Popup, Sort};
use super::view;
use crate::config::Config;
use crate::scan::ExcludeKind;

struct Fixture {
    _tmp: tempfile::TempDir,
    root: PathBuf,
    config: PathBuf,
    data: PathBuf,
}

fn write(path: &Path, bytes: usize) {
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    std::fs::write(path, vec![b'x'; bytes]).unwrap();
}

impl Fixture {
    fn new() -> Fixture {
        let tmp = tempfile::tempdir().unwrap();
        let base = std::fs::canonicalize(tmp.path()).unwrap();
        let root = base.join("root");
        write(&root.join("alpha/big.dat"), 50_000);
        write(&root.join("alpha/sub/a.csv"), 1000);
        write(&root.join("alpha/venv/pyvenv.cfg"), 10);
        write(&root.join("beta/b.txt"), 20_000);
        write(&root.join("loose/l.txt"), 5000);
        write(&root.join("projects/gamma/g.txt"), 30_000);
        write(&root.join("archived/.stomp-archive/X-1/journal.jsonl"), 0);
        write(&root.join("notes.txt"), 100);
        let config = base.join("config.toml");
        std::fs::write(
            &config,
            format!(
                r#"# test config
[general]
roots = ["{r}"]

[[grant]]
id = "A"
dirs = ["{r}/alpha"]

[[grant]]
id = "B"   # keep this comment
dirs = ["{r}/beta"]

[[grant]]
id = "C"
dirs = ["{r}/projects/gamma"]
"#,
                r = root.display()
            ),
        )
        .unwrap();
        Fixture {
            _tmp: tmp,
            root,
            config,
            data: base.join("state"),
        }
    }

    fn app(&self) -> App {
        let cfg = Config::load(&self.config).unwrap();
        let mut app = App::new(cfg, &self.config, &self.data, None, false).unwrap();
        app.measure_pending();
        app
    }
}

fn key(app: &mut App, code: KeyCode) -> Option<Command> {
    let c = app.key(KeyEvent::new(code, KeyModifiers::NONE));
    app.measure_pending();
    c
}

fn select(app: &mut App, name: &str) {
    app.selected = app.entries.iter().position(|e| e.name == name).unwrap();
}

fn names(app: &App) -> Vec<&str> {
    app.entries.iter().map(|e| e.name.as_str()).collect()
}

fn screen(app: &mut App) -> String {
    let mut terminal = Terminal::new(TestBackend::new(140, 30)).unwrap();
    terminal.draw(|f| view::draw(f, app)).unwrap();
    let buffer = terminal.backend().buffer();
    let mut out = String::new();
    for y in 0..buffer.area.height {
        for x in 0..buffer.area.width {
            out += buffer[(x, y)].symbol();
        }
        out.push('\n');
    }
    out
}

#[test]
fn lists_measures_and_marks() {
    let f = Fixture::new();
    let app = f.app();
    assert_eq!(app.location, Location::Dir(f.root.clone()));
    // Largest first; the file comes last.
    assert_eq!(
        names(&app),
        [
            "alpha",
            "projects",
            "beta",
            "loose",
            "notes.txt",
            "archived"
        ]
    );
    let alpha = &app.entries[0];
    assert_eq!(alpha.grant.as_deref(), Some("A"));
    assert_eq!(alpha.usage.unwrap().bytes, 51_010);
    assert_eq!(alpha.usage.unwrap().files, 3);
    let projects = app.entries.iter().find(|e| e.name == "projects").unwrap();
    assert_eq!(projects.marks, [Mark::HoldsGrants(vec!["C".into()])]);
    assert_eq!(projects.grant, None);
    let archived = app.entries.iter().find(|e| e.name == "archived").unwrap();
    assert!(archived.is_archived());
    assert_eq!(
        app.entries
            .iter()
            .find(|e| e.name == "notes.txt")
            .unwrap()
            .kind,
        Kind::File
    );
}

#[test]
fn navigates_and_sorts() {
    let f = Fixture::new();
    let mut app = f.app();
    select(&mut app, "alpha");
    key(&mut app, KeyCode::Enter);
    assert_eq!(app.error, None);
    assert_eq!(app.location, Location::Dir(app.cfg.roots[0].join("alpha")));
    assert_eq!(names(&app), ["big.dat", "sub", "venv"]);
    let venv = app.entries.iter().find(|e| e.name == "venv").unwrap();
    assert_eq!(venv.marks, [Mark::Excluded(ExcludeKind::Environment)]);
    // Back up lands on the directory we came from.
    key(&mut app, KeyCode::Left);
    assert_eq!(app.selected_entry().unwrap().name, "alpha");
    // The root is the top.
    key(&mut app, KeyCode::Left);
    assert!(app.status.as_deref().unwrap().contains("top"));

    key(&mut app, KeyCode::Char('s'));
    assert_eq!(app.sort, Sort::Name);
    assert_eq!(names(&app)[0], "alpha");
    assert_eq!(
        app.selected_entry().unwrap().name,
        "alpha",
        "selection follows the entry"
    );
    key(&mut app, KeyCode::Char('s'));
    assert_eq!(app.sort, Sort::Files);
    assert_eq!(names(&app)[0], "alpha");
}

#[test]
fn assigns_directories_to_grants_in_the_config() {
    let f = Fixture::new();
    let mut app = f.app();
    select(&mut app, "loose");
    key(&mut app, KeyCode::Char('g'));
    let Some(Popup::Grants { options, .. }) = &app.popup else {
        panic!("no grant picker");
    };
    let labels: Vec<&str> = options.iter().map(|o| o.0.as_str()).collect();
    assert_eq!(labels, ["A", "B", "C"]);
    key(&mut app, KeyCode::Down);
    key(&mut app, KeyCode::Enter);
    assert!(
        app.status.as_deref().unwrap().starts_with("Added"),
        "{:?}",
        app.status
    );
    let loose = app.entries.iter().find(|e| e.name == "loose").unwrap();
    assert_eq!(loose.grant.as_deref(), Some("B"));
    let text = std::fs::read_to_string(&f.config).unwrap();
    assert!(text.contains("# keep this comment"));
    assert!(
        text.contains(&format!("\"{}/loose\"", f.root.display())),
        "{text}"
    );
    assert!(PathBuf::from(format!("{}.bak", f.config.display())).exists());

    // Now it offers removal first.
    select(&mut app, "loose");
    key(&mut app, KeyCode::Char('g'));
    let Some(Popup::Grants { options, .. }) = &app.popup else {
        panic!("no grant picker");
    };
    assert_eq!(options[0].0, "Remove from grant B");
    key(&mut app, KeyCode::Enter);
    assert_eq!(
        app.entries
            .iter()
            .find(|e| e.name == "loose")
            .unwrap()
            .grant,
        None
    );

    // A directory inside another grant's can't be given to a second grant.
    let alpha_sub = f.root.join("alpha/sub");
    let err = app
        .set_grant(&alpha_sub, Some("B"))
        .unwrap_err()
        .to_string();
    assert!(err.contains("wasn't saved"), "{err}");
    let err = app
        .set_grant(&alpha_sub, Some("A"))
        .unwrap_err()
        .to_string();
    assert!(err.contains("already counts toward grant A"), "{err}");
}

#[test]
fn actions_are_offered_only_where_they_apply() {
    let f = Fixture::new();
    let mut app = f.app();
    select(&mut app, "beta");
    assert_eq!(
        key(&mut app, KeyCode::Char('a')),
        Some(Command::Archive(f.root.join("beta")))
    );
    assert_eq!(
        key(&mut app, KeyCode::Char('e')),
        Some(Command::Estimate(f.root.join("beta")))
    );
    assert_eq!(key(&mut app, KeyCode::Char('r')), None);
    assert!(matches!(app.popup, Some(Popup::Message { .. })));
    key(&mut app, KeyCode::Esc); // closes the popup only
    assert!(app.popup.is_none());

    select(&mut app, "archived");
    assert_eq!(
        key(&mut app, KeyCode::Char('r')),
        Some(Command::Restore(f.root.join("archived")))
    );
    assert_eq!(key(&mut app, KeyCode::Char('a')), None, "already archived");
    key(&mut app, KeyCode::Esc);

    // stomp's own checks run before leaving the screen.
    select(&mut app, "projects");
    assert_eq!(key(&mut app, KeyCode::Char('a')), None);
    let Some(Popup::Message { text, .. }) = &app.popup else {
        panic!("no message");
    };
    assert!(text.contains("grant(s) C"), "{text}");
    key(&mut app, KeyCode::Esc);

    select(&mut app, "notes.txt");
    assert_eq!(key(&mut app, KeyCode::Char('a')), None);
    key(&mut app, KeyCode::Esc);
    assert_eq!(key(&mut app, KeyCode::Char('q')), Some(Command::Quit));
}

#[test]
fn draws_the_listing_and_popups() {
    let f = Fixture::new();
    let mut app = f.app();
    let text = screen(&mut app);
    assert!(text.contains(" stomp "), "{text}");
    assert!(text.contains("alpha/"));
    assert!(text.contains("[grants C]"));
    assert!(text.contains("[archiving X-1]"));
    assert!(text.contains("49.8 KiB"), "{text}");
    assert!(text.contains("sorted by size"));

    select(&mut app, "alpha");
    key(&mut app, KeyCode::Char('i'));
    let text = screen(&mut app);
    assert!(text.contains("Grant     A"), "{text}");
    assert!(text.contains("No history yet"), "{text}");
    key(&mut app, KeyCode::Esc);

    key(&mut app, KeyCode::Char('?'));
    assert!(screen(&mut app).contains("Moving"));
    key(&mut app, KeyCode::Esc);

    key(&mut app, KeyCode::Char('c'));
    assert!(screen(&mut app).contains("No jobs in the last 30 days"));
}
