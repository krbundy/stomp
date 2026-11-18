//! The TUI's state: where the user is, what's listed, and what each key does.
//! Directory sizes are measured off the main thread (instant on CephFS, a
//! walk elsewhere) and cached, so moving around stays responsive.

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::mpsc::{Receiver, Sender, channel};

use anyhow::{Result, bail};
use jiff::Timestamp;
use ratatui::crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use ratatui::widgets::TableState;

use super::edit;
use crate::archive::{self, ARCHIVE_DIR};
use crate::catalog::Catalog;
use crate::config::Config;
use crate::fs::{Quota, Usage};
use crate::scan::ExcludeKind;
use crate::scan::rules::Rules;
use crate::units::{format_bytes, format_count};
use crate::{compute, paths};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Kind {
    Dir,
    File,
    Link,
    Other,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Mark {
    Excluded(ExcludeKind),
    /// Holds a stomp archive; `finished` is false while archiving is under way.
    Archived {
        id: String,
        finished: bool,
    },
    /// stomp's own `.stomp-archive` directory.
    ArchiveData,
    /// Contains these grants' directories.
    HoldsGrants(Vec<String>),
}

#[derive(Debug, Clone)]
pub struct Entry {
    pub name: String,
    pub path: PathBuf,
    pub kind: Kind,
    /// `None` for a directory until it has been measured.
    pub usage: Option<Usage>,
    pub grant: Option<String>,
    pub marks: Vec<Mark>,
    pub proposal: Option<i64>,
}

impl Entry {
    pub fn is_archived(&self) -> bool {
        self.marks
            .iter()
            .any(|m| matches!(m, Mark::Archived { .. }))
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Sort {
    Size,
    Name,
    Files,
    /// Least recently changed first.
    Age,
}

impl Sort {
    pub fn label(self) -> &'static str {
        match self {
            Sort::Size => "size",
            Sort::Name => "name",
            Sort::Files => "files",
            Sort::Age => "oldest first",
        }
    }

    fn next(self) -> Sort {
        match self {
            Sort::Size => Sort::Name,
            Sort::Name => Sort::Files,
            Sort::Files => Sort::Age,
            Sort::Age => Sort::Size,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Location {
    /// The configured roots, when there are several.
    Roots,
    Dir(PathBuf),
}

/// Work done with the browser suspended, through the CLI's own commands.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Command {
    Quit,
    Estimate(PathBuf),
    Archive(PathBuf),
    Restore(PathBuf),
}

#[derive(Debug, Clone)]
pub enum Popup {
    Help,
    Message {
        title: String,
        text: String,
    },
    Info {
        title: String,
        lines: Vec<String>,
    },
    Grants {
        path: PathBuf,
        /// (label, grant to add to, or None to remove from the current one)
        options: Vec<(String, Option<String>)>,
        selected: usize,
    },
}

/// One line of the compute panel: a grant's jobs over the last 30 days.
#[derive(Debug, Clone)]
pub struct ComputeRow {
    pub grant: String,
    pub jobs: u64,
    pub cpu_hours: f64,
    pub gpu_hours: f64,
}

pub struct ComputePanel {
    pub rows: Vec<ComputeRow>,
    pub synced: Option<i64>,
}

/// What measuring a directory found.
#[derive(Debug, Clone)]
pub struct Measured {
    pub path: PathBuf,
    pub usage: Option<Usage>,
    pub environment: bool,
    pub archive: Option<(String, bool)>,
}

/// Measures a directory: its subtree totals, whether it's a software
/// environment, and whether it holds an archive.
pub fn measure(path: &Path) -> Measured {
    let usage = crate::fs::detect(path).and_then(|b| b.usage(path)).ok();
    let environment =
        crate::fs::read_entries(path).is_ok_and(|(entries, _)| Rules::is_environment(&entries));
    let archive = archive::find_in(path)
        .into_iter()
        .find(|f| !f.restored)
        .map(|f| (f.id, f.finished));
    Measured {
        path: path.to_path_buf(),
        usage,
        environment,
        archive,
    }
}

/// Measures directories on a background thread, newest request first.
struct Worker {
    requests: Sender<(u64, Vec<PathBuf>)>,
    results: Receiver<(u64, Measured)>,
    generation: Arc<AtomicU64>,
}

impl Worker {
    fn start() -> Worker {
        let (requests, incoming) = channel::<(u64, Vec<PathBuf>)>();
        let (outgoing, results) = channel();
        let generation = Arc::new(AtomicU64::new(0));
        let current = generation.clone();
        std::thread::spawn(move || {
            while let Ok((n, paths)) = incoming.recv() {
                for path in paths {
                    // A newer request supersedes this one.
                    if current.load(Ordering::Relaxed) != n {
                        break;
                    }
                    if outgoing.send((n, measure(&path))).is_err() {
                        return;
                    }
                }
            }
        });
        Worker {
            requests,
            results,
            generation,
        }
    }

    fn request(&self, paths: Vec<PathBuf>) -> u64 {
        let n = self.generation.fetch_add(1, Ordering::Relaxed) + 1;
        let _ = self.requests.send((n, paths));
        n
    }
}

pub struct App {
    pub cfg: Config,
    pub config_path: PathBuf,
    pub data_dir: PathBuf,
    pub location: Location,
    pub entries: Vec<Entry>,
    pub selected: usize,
    /// Whether the selection follows its entry when the list is re-sorted.
    /// A fresh listing keeps the top row selected while sizes arrive, until
    /// the user moves; otherwise a file, whose size is known at once, would
    /// start out selected and drift down the list as directories are measured.
    pub follow: bool,
    pub table: TableState,
    pub sort: Sort,
    pub popup: Option<Popup>,
    pub compute: Option<ComputePanel>,
    pub status: Option<String>,
    pub quota: Option<Quota>,
    /// Whether sizes here come without walking (CephFS).
    pub instant: bool,
    pub error: Option<String>,
    /// Rows a page-up or page-down moves; set when drawing.
    pub page: usize,
    rules: Rules,
    cache: HashMap<PathBuf, Measured>,
    pub pending: HashSet<PathBuf>,
    proposals: HashMap<PathBuf, i64>,
    worker: Option<Worker>,
    generation: u64,
}

fn now() -> i64 {
    Timestamp::now().as_second()
}

impl App {
    /// `background` measures on a thread; without it (in tests), call
    /// [`App::measure_pending`].
    pub fn new(
        cfg: Config,
        config_path: &Path,
        data_dir: &Path,
        start: Option<&Path>,
        background: bool,
    ) -> Result<App> {
        let location = match start {
            Some(dir) => {
                let dir = std::fs::canonicalize(dir)?;
                if !cfg.roots.iter().any(|r| dir.starts_with(r)) {
                    bail!(
                        "{} isn't inside any of your roots (general.roots)",
                        paths::display(&dir)
                    );
                }
                Location::Dir(dir)
            }
            None if cfg.roots.len() == 1 => Location::Dir(cfg.roots[0].clone()),
            None => Location::Roots,
        };
        let rules = Rules::new(&cfg, data_dir)?;
        let mut app = App {
            cfg,
            config_path: config_path.to_path_buf(),
            data_dir: data_dir.to_path_buf(),
            location,
            entries: Vec::new(),
            selected: 0,
            follow: false,
            table: TableState::default(),
            sort: Sort::Size,
            popup: None,
            compute: None,
            status: None,
            quota: None,
            instant: true,
            error: None,
            page: 20,
            rules,
            cache: HashMap::new(),
            pending: HashSet::new(),
            proposals: HashMap::new(),
            worker: background.then(Worker::start),
            generation: 0,
        };
        app.load_proposals();
        app.load();
        Ok(app)
    }

    /// Open archive proposals, so they can be marked.
    fn load_proposals(&mut self) {
        self.proposals = Catalog::open(&self.data_dir)
            .and_then(|c| c.proposals(false))
            .map(|ps| ps.into_iter().map(|p| (p.path, p.id)).collect())
            .unwrap_or_default();
    }

    pub fn selected_entry(&self) -> Option<&Entry> {
        self.entries.get(self.selected)
    }

    fn entry_for(&self, path: &Path, name: String, kind: Kind) -> Entry {
        let mut marks = Vec::new();
        if kind == Kind::Dir {
            if name == ARCHIVE_DIR {
                marks.push(Mark::ArchiveData);
            } else if let Some(kind) = self.rules.by_path(path) {
                marks.push(Mark::Excluded(kind));
            }
            let within: Vec<String> = self
                .cfg
                .grants_within(path)
                .iter()
                .map(|g| g.id.clone())
                .collect();
            if !within.is_empty() {
                marks.push(Mark::HoldsGrants(within));
            }
        }
        Entry {
            name,
            path: path.to_path_buf(),
            kind,
            usage: None,
            grant: self.cfg.grant_for(path).map(|g| g.id.clone()),
            marks,
            proposal: self.proposals.get(path).copied(),
        }
    }

    fn list(&self) -> Result<Vec<Entry>> {
        match &self.location {
            Location::Roots => Ok(self
                .cfg
                .roots
                .iter()
                .map(|r| self.entry_for(r, paths::display(r), Kind::Dir))
                .collect()),
            Location::Dir(dir) => {
                let mut out = Vec::new();
                for e in std::fs::read_dir(dir)? {
                    let Ok(e) = e else { continue };
                    let name = e.file_name().to_string_lossy().into_owned();
                    let Ok(md) = e.metadata() else { continue };
                    let ft = md.file_type();
                    let kind = if ft.is_dir() {
                        Kind::Dir
                    } else if ft.is_symlink() {
                        Kind::Link
                    } else if ft.is_file() {
                        Kind::File
                    } else {
                        Kind::Other
                    };
                    let mut entry = self.entry_for(&e.path(), name, kind);
                    if kind != Kind::Dir {
                        use std::os::unix::fs::MetadataExt;
                        entry.usage = Some(Usage {
                            bytes: if kind == Kind::File { md.len() } else { 0 },
                            files: 1,
                            newest_change: Some(md.mtime().max(md.ctime())),
                            unreadable: 0,
                        });
                    }
                    out.push(entry);
                }
                Ok(out)
            }
        }
    }

    /// Lists the current location and starts measuring what isn't cached.
    pub fn load(&mut self) {
        let keep = self.selected_entry().map(|e| e.path.clone());
        self.error = None;
        self.entries = match self.list() {
            Ok(entries) => entries,
            Err(e) => {
                self.error = Some(format!("can't list this directory: {e}"));
                Vec::new()
            }
        };
        let mut todo = Vec::new();
        for i in 0..self.entries.len() {
            if self.entries[i].kind != Kind::Dir {
                continue;
            }
            let path = self.entries[i].path.clone();
            match self.cache.get(&path).cloned() {
                Some(m) => self.apply(m),
                None => todo.push(path),
            }
        }
        self.pending = todo.iter().cloned().collect();
        if let Some(w) = &self.worker {
            self.generation = w.request(todo);
        }
        let here = match &self.location {
            Location::Dir(d) => Some(d.clone()),
            Location::Roots => None,
        };
        match here.as_deref().map(crate::fs::detect) {
            Some(Ok(backend)) => {
                self.instant = backend.is_instant();
                self.quota = here
                    .as_deref()
                    .and_then(|d| backend.quota(d).ok().flatten());
            }
            _ => {
                self.instant = true;
                self.quota = None;
            }
        }
        self.sort_entries(keep.as_deref());
    }

    fn apply(&mut self, m: Measured) {
        let Some(e) = self.entries.iter_mut().find(|e| e.path == m.path) else {
            return;
        };
        e.usage = m.usage;
        if m.environment && !e.marks.iter().any(|x| matches!(x, Mark::Excluded(_))) {
            e.marks.push(Mark::Excluded(ExcludeKind::Environment));
        }
        e.marks.retain(|x| !matches!(x, Mark::Archived { .. }));
        if let Some((id, finished)) = m.archive {
            e.marks.push(Mark::Archived { id, finished });
        }
    }

    fn accept(&mut self, m: Measured) {
        self.pending.remove(&m.path);
        self.cache.insert(m.path.clone(), m.clone());
        self.apply(m);
    }

    /// Takes in what the background measurer has finished. Returns whether anything changed.
    pub fn drain(&mut self) -> bool {
        let mut got = Vec::new();
        if let Some(w) = &self.worker {
            while let Ok((n, m)) = w.results.try_recv() {
                if n == self.generation {
                    got.push(m);
                } else {
                    // Still worth keeping for later visits.
                    self.cache.insert(m.path.clone(), m);
                }
            }
        }
        let changed = !got.is_empty();
        for m in got {
            self.accept(m);
        }
        if changed {
            let keep = self.selected_entry().map(|e| e.path.clone());
            self.sort_entries(keep.as_deref());
        }
        changed
    }

    /// Measures everything pending, here and now (tests).
    #[cfg(test)]
    pub fn measure_pending(&mut self) {
        let todo: Vec<PathBuf> = self.pending.iter().cloned().collect();
        for p in todo {
            self.accept(measure(&p));
        }
        let keep = self.selected_entry().map(|e| e.path.clone());
        self.sort_entries(keep.as_deref());
    }

    fn sort_entries(&mut self, keep: Option<&Path>) {
        let size = |e: &Entry| e.usage.map(|u| u.bytes);
        match self.sort {
            Sort::Size => self
                .entries
                .sort_by(|a, b| size(b).cmp(&size(a)).then_with(|| a.name.cmp(&b.name))),
            Sort::Name => self.entries.sort_by(|a, b| a.name.cmp(&b.name)),
            Sort::Files => self.entries.sort_by(|a, b| {
                let f = |e: &Entry| e.usage.map(|u| u.files);
                f(b).cmp(&f(a)).then_with(|| a.name.cmp(&b.name))
            }),
            Sort::Age => self.entries.sort_by(|a, b| {
                // Unknown ages last.
                let t = |e: &Entry| e.usage.and_then(|u| u.newest_change).unwrap_or(i64::MAX);
                t(a).cmp(&t(b)).then_with(|| a.name.cmp(&b.name))
            }),
        }
        let kept = keep
            .filter(|_| self.follow)
            .and_then(|p| self.entries.iter().position(|e| e.path == p));
        self.selected = kept.unwrap_or(0).min(self.entries.len().saturating_sub(1));
    }

    /// Moves the selection at the user's request; from then on it follows its entry.
    fn select(&mut self, i: usize) {
        self.selected = i;
        self.follow = true;
    }

    fn go(&mut self, location: Location, select: Option<PathBuf>) {
        self.location = location;
        self.selected = 0;
        // Coming back up lands on the directory we left, and stays on it.
        self.follow = select.is_some();
        self.table = TableState::default();
        self.load();
        if let Some(p) = select
            && let Some(i) = self.entries.iter().position(|e| e.path == p)
        {
            self.selected = i;
        }
    }

    fn open(&mut self) {
        let Some(e) = self.selected_entry() else {
            return;
        };
        if e.kind != Kind::Dir {
            return;
        }
        let path = e.path.clone();
        self.go(Location::Dir(path), None);
    }

    fn up(&mut self) {
        let Location::Dir(dir) = &self.location else {
            return;
        };
        let dir = dir.clone();
        if self.cfg.roots.contains(&dir) {
            if self.cfg.roots.len() > 1 {
                self.go(Location::Roots, Some(dir));
            } else {
                self.status = Some("This is the top of your root.".into());
            }
            return;
        }
        if let Some(parent) = dir.parent() {
            self.go(Location::Dir(parent.to_path_buf()), Some(dir));
        }
    }

    /// Measures the listed directories again.
    pub fn refresh(&mut self) {
        for e in &self.entries {
            self.cache.remove(&e.path);
        }
        self.load_proposals();
        self.load();
    }

    /// After a suspended command: things may have been archived or restored.
    pub fn after_command(&mut self) {
        self.refresh();
    }

    fn message(&mut self, title: &str, text: impl Into<String>) {
        self.popup = Some(Popup::Message {
            title: title.into(),
            text: text.into(),
        });
    }

    /// The selected directory, if an archive-related action may apply to it.
    fn target(&mut self, action: &str) -> Option<Entry> {
        if self.location == Location::Roots {
            self.message(action, "Open a root and pick a directory inside it.");
            return None;
        }
        match self.selected_entry().cloned() {
            Some(e) if e.kind == Kind::Dir => Some(e),
            _ => {
                self.message(action, "Pick a directory.");
                None
            }
        }
    }

    fn archive_target(&mut self, action: &str) -> Option<PathBuf> {
        let e = self.target(action)?;
        if e.is_archived() {
            self.message(action, "This directory is already archived; r restores it.");
            return None;
        }
        match archive::check_target(&self.cfg, &e.path, &self.data_dir, false) {
            Ok(()) => Some(e.path),
            Err(err) => {
                self.message(action, format!("{err:#}"));
                None
            }
        }
    }

    fn grant_picker(&mut self) {
        let Some(e) = self.target("Grant") else {
            return;
        };
        let mut options = Vec::new();
        let listed = self
            .cfg
            .grants
            .iter()
            .find(|g| g.dirs.iter().any(|d| d.path == e.path));
        if let Some(g) = listed {
            options.push((format!("Remove from grant {}", g.id), None));
        }
        for g in &self.cfg.grants {
            if listed.is_some_and(|l| l.id == g.id) {
                continue;
            }
            let label = match &g.title {
                Some(t) => format!("{}  {t}", g.id),
                None => g.id.clone(),
            };
            options.push((label, Some(g.id.clone())));
        }
        if options.is_empty() {
            self.message(
                "Grant",
                "There are no grants in the config yet; add a [[grant]] block first.",
            );
            return;
        }
        self.popup = Some(Popup::Grants {
            path: e.path,
            options,
            selected: 0,
        });
    }

    /// Adds `path` to `grant`, or removes it from its grant with `None`,
    /// by editing the config file.
    pub fn set_grant(&mut self, path: &Path, grant: Option<&str>) -> Result<String> {
        let text = std::fs::read_to_string(&self.config_path)?;
        let shown = paths::display(path);
        let (new_text, done) = match grant {
            Some(id) => {
                if let Some(g) = self.cfg.grant_for(path)
                    && g.id == id
                {
                    bail!("{shown} already counts toward grant {id} through a directory above it");
                }
                (
                    edit::assign(&text, id, &shown)?,
                    format!("Added {shown} to grant {id}"),
                )
            }
            None => {
                let (t, id) = edit::unassign(&text, path)?;
                (t, format!("Removed {shown} from grant {id}"))
            }
        };
        let (cfg, backup) = edit::save(&self.config_path, &new_text)?;
        self.rules = Rules::new(&cfg, &self.data_dir)?;
        self.cfg = cfg;
        self.load();
        Ok(format!(
            "{done} (the previous config is {}).",
            paths::display(&backup)
        ))
    }

    fn info(&mut self) {
        let Some(e) = self.selected_entry().cloned() else {
            return;
        };
        let mut lines = vec![format!("Path      {}", paths::display(&e.path))];
        match e.grant.as_deref().and_then(|id| self.cfg.grant(id)) {
            Some(g) => {
                let mut text = format!("Grant     {}", g.id);
                if let Some(t) = &g.title {
                    text += &format!(" — {t}");
                }
                if let Some(end) = g.end {
                    text += &format!(" (ends {end})");
                }
                lines.push(text);
            }
            None => lines.push("Grant     none (unassigned)".into()),
        }
        if let Some(u) = e.usage {
            lines.push(if e.kind == Kind::Dir {
                format!(
                    "Size      {} in {} files",
                    format_bytes(u.bytes),
                    format_count(u.files)
                )
            } else {
                format!("Size      {}", format_bytes(u.bytes))
            });
            if let Some(t) = u.newest_change {
                lines.push(format!("Changed   {}", crate::cmd::age(Some(t))));
            }
        }
        for m in &e.marks {
            lines.push(format!("Note      {}", mark_text(m)));
        }
        if let Some(id) = e.proposal {
            lines.push(format!(
                "Proposed  for archiving as #{id}; `stomp candidates` says why"
            ));
        }
        if let Some(Mark::Archived { id, .. }) =
            e.marks.iter().find(|m| matches!(m, Mark::Archived { .. }))
            && let Ok(s) = archive::load_settings(&archive::archive_path(&e.path, id))
        {
            lines.push(format!("Archive   {id}, chunks at {}", s.destination));
        }
        match self.history(&e.path) {
            Ok(h) => lines.extend(h),
            Err(err) => lines.push(format!("\n(no history: {err:#})")),
        }
        self.popup = Some(Popup::Info {
            title: e.name,
            lines,
        });
    }

    /// Sizes recorded by scans and usage samples.
    fn history(&self, path: &Path) -> Result<Vec<String>> {
        let catalog = Catalog::open(&self.data_dir)?;
        let mut lines = Vec::new();
        let mut scan = catalog.latest_scan(true)?;
        let mut rows = Vec::new();
        while let Some(s) = scan {
            if rows.len() == 8 {
                break;
            }
            if let Some(r) = catalog.dir_row(s.id, path)? {
                rows.push(format!(
                    "  {}  {:>10}  {:>12} files",
                    crate::cmd::local_date(s.started_at),
                    format_bytes(r.agg.bytes),
                    format_count(r.agg.files)
                ));
            }
            scan = catalog.scan_before(&s, true)?;
        }
        if !rows.is_empty() {
            lines.push(String::new());
            lines.push("Scans".into());
            lines.extend(rows);
        }
        let shown = path.to_string_lossy();
        let mut by_day: Vec<(String, u64, u64)> = Vec::new();
        for s in catalog.samples(Some(now() - 90 * 86_400), None)? {
            if s.path != shown || s.kind == "unassigned" {
                continue;
            }
            let day = crate::cmd::local_date(s.taken_at);
            match by_day.last_mut() {
                Some(last) if last.0 == day => *last = (day, s.bytes, s.files),
                _ => by_day.push((day, s.bytes, s.files)),
            }
        }
        if !by_day.is_empty() {
            lines.push(String::new());
            lines.push("Usage samples".into());
            for (day, bytes, files) in by_day.iter().rev().take(10) {
                lines.push(format!(
                    "  {day}  {:>10}  {:>12} files",
                    format_bytes(*bytes),
                    format_count(*files)
                ));
            }
        }
        if lines.is_empty() {
            lines.push(String::new());
            lines.push(
                "No history yet: scans record directories, and usage samples record roots and grant directories."
                    .into(),
            );
        }
        Ok(lines)
    }

    fn toggle_compute(&mut self) {
        if self.compute.take().is_some() {
            return;
        }
        match load_compute(&self.cfg, &self.data_dir) {
            Ok(panel) => self.compute = Some(panel),
            Err(e) => self.message("Compute", format!("{e:#}")),
        }
    }

    /// Handles a key; returns a command for the caller to carry out.
    pub fn key(&mut self, key: KeyEvent) -> Option<Command> {
        if key.code == KeyCode::Char('c') && key.modifiers.contains(KeyModifiers::CONTROL) {
            return Some(Command::Quit);
        }
        self.status = None;
        if let Some(popup) = self.popup.take() {
            return self.popup_key(popup, key);
        }
        let last = self.entries.len().saturating_sub(1);
        match key.code {
            KeyCode::Char('q') | KeyCode::Esc => return Some(Command::Quit),
            KeyCode::Down | KeyCode::Char('j') => self.select((self.selected + 1).min(last)),
            KeyCode::Up | KeyCode::Char('k') => self.select(self.selected.saturating_sub(1)),
            KeyCode::PageDown => self.select((self.selected + self.page).min(last)),
            KeyCode::PageUp => self.select(self.selected.saturating_sub(self.page)),
            KeyCode::Home => self.select(0),
            KeyCode::End => self.select(last),
            KeyCode::Right | KeyCode::Enter | KeyCode::Char('l') => self.open(),
            KeyCode::Left | KeyCode::Backspace | KeyCode::Char('h') => self.up(),
            KeyCode::Char('s') => {
                self.sort = self.sort.next();
                let keep = self.selected_entry().map(|e| e.path.clone());
                self.sort_entries(keep.as_deref());
                self.status = Some(format!("Sorted by {}.", self.sort.label()));
            }
            KeyCode::Char('R') => self.refresh(),
            KeyCode::Char('g') => self.grant_picker(),
            KeyCode::Char('i') => self.info(),
            KeyCode::Char('c') => self.toggle_compute(),
            KeyCode::Char('?') => self.popup = Some(Popup::Help),
            KeyCode::Char('e') => return self.archive_target("Estimate").map(Command::Estimate),
            KeyCode::Char('a') => return self.archive_target("Archive").map(Command::Archive),
            KeyCode::Char('r') => {
                let e = self.target("Restore")?;
                if e.is_archived() {
                    return Some(Command::Restore(e.path));
                }
                self.message("Restore", "This directory isn't archived.");
            }
            _ => {}
        }
        None
    }

    fn popup_key(&mut self, popup: Popup, key: KeyEvent) -> Option<Command> {
        let Popup::Grants {
            path,
            options,
            mut selected,
        } = popup
        else {
            // Any key closes the other popups.
            return None;
        };
        match key.code {
            KeyCode::Down | KeyCode::Char('j') => selected = (selected + 1).min(options.len() - 1),
            KeyCode::Up | KeyCode::Char('k') => selected = selected.saturating_sub(1),
            KeyCode::Enter => {
                let grant = options[selected].1.clone();
                match self.set_grant(&path, grant.as_deref()) {
                    Ok(text) => self.status = Some(text),
                    Err(e) => self.message("Grant", format!("{e:#}")),
                }
                return None;
            }
            KeyCode::Esc | KeyCode::Char('q') => return None,
            _ => {}
        }
        self.popup = Some(Popup::Grants {
            path,
            options,
            selected,
        });
        None
    }

    /// The grant of the current location, if any.
    pub fn here_grant(&self) -> Option<&crate::config::Grant> {
        match &self.location {
            Location::Dir(d) => self.cfg.grant_for(d),
            Location::Roots => None,
        }
    }
}

/// Short text for a mark, as shown beside a name.
pub fn mark_text(m: &Mark) -> String {
    match m {
        Mark::Excluded(ExcludeKind::Environment) => "environment".into(),
        Mark::Excluded(ExcludeKind::Cache) => "cache".into(),
        Mark::Excluded(ExcludeKind::User) => "excluded".into(),
        Mark::Excluded(ExcludeKind::Stomp) => "stomp data".into(),
        Mark::Archived { id, finished: true } => format!("archived {id}"),
        Mark::Archived {
            id,
            finished: false,
        } => format!("archiving {id}"),
        Mark::ArchiveData => "archive data".into(),
        Mark::HoldsGrants(ids) => format!("grants {}", ids.join(", ")),
    }
}

/// Jobs per grant over the last 30 days, from the catalog.
fn load_compute(cfg: &Config, data_dir: &Path) -> Result<ComputePanel> {
    let catalog = Catalog::open(data_dir)?;
    let synced = compute::last_sync(&catalog)?;
    let jobs = catalog.jobs_in_range(Some(now() - 30 * 86_400), None)?;
    let attributor = compute::Attributor::new(cfg, catalog.tags()?);
    let mut by_grant: HashMap<String, ComputeRow> = HashMap::new();
    for job in &jobs {
        let grant = attributor.attribute(job).0;
        let row = by_grant.entry(grant.clone()).or_insert(ComputeRow {
            grant,
            jobs: 0,
            cpu_hours: 0.0,
            gpu_hours: 0.0,
        });
        row.jobs += 1;
        row.cpu_hours += job.cpu_hours();
        row.gpu_hours += job.gpu_hours();
    }
    let mut rows: Vec<ComputeRow> = by_grant.into_values().collect();
    rows.sort_by(|a, b| b.cpu_hours.total_cmp(&a.cpu_hours));
    Ok(ComputePanel { rows, synced })
}
