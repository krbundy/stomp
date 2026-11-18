//! Scanner tests: the walker on real temporary trees and on a fake CephFS,
//! interruption and resumption, and a full scan imported into the catalog.

use std::collections::HashMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};

use super::walk::{FileStats, PrevRows, Progress, Stop, UnitStore, Walked, Walker};
use super::*;
use crate::catalog::Catalog;
use crate::fs::ceph::{CephBackend, fake::FakeXattrs};
use crate::fs::posix::PosixBackend;

fn write(path: &Path, len: usize) {
    fs::create_dir_all(path.parent().unwrap()).unwrap();
    fs::write(path, vec![b'x'; len]).unwrap();
}

/// Units kept in memory. Optionally sets `stop` after a number of commits.
#[derive(Default)]
struct MemoryUnits {
    done: Mutex<HashMap<PathBuf, (Walked, Vec<DirRow>)>>,
    commits: AtomicUsize,
    stop_after: Option<(usize, &'static AtomicBool)>,
}

impl UnitStore for MemoryUnits {
    fn done(&self, path: &Path) -> Option<Walked> {
        self.done.lock().unwrap().get(path).map(|(w, _)| w.clone())
    }

    fn commit(&self, unit: &Walked, rows: Vec<DirRow>) -> anyhow::Result<()> {
        self.done
            .lock()
            .unwrap()
            .insert(unit.row.path.clone(), (unit.clone(), rows));
        let n = self.commits.fetch_add(1, Ordering::SeqCst) + 1;
        if let Some((limit, stop)) = self.stop_after
            && n >= limit
        {
            stop.store(true, Ordering::SeqCst);
        }
        Ok(())
    }
}

impl MemoryUnits {
    fn rows(&self) -> Vec<DirRow> {
        self.done
            .lock()
            .unwrap()
            .values()
            .flat_map(|(_, rows)| rows.clone())
            .collect()
    }
}

struct Setup {
    _tmp: tempfile::TempDir,
    root: PathBuf,
    rules: Rules,
    grants: HashMap<PathBuf, String>,
}

/// root/
///   alpha/                 grant A
///     big/data.bin         2 MiB, kept as its own row
///     small/a.txt          10 B, folded into alpha
///     env/pyvenv.cfg       a Python venv: excluded, not walked
///   proj/node_modules/x    excluded by the user's glob
///   home/.cache/pip/w      a known cache
///   loose.txt
fn setup() -> Setup {
    let tmp = tempfile::tempdir().unwrap();
    let root = fs::canonicalize(tmp.path()).unwrap().join("root");
    write(&root.join("alpha/big/data.bin"), 2 << 20);
    write(&root.join("alpha/small/a.txt"), 10);
    write(&root.join("alpha/env/pyvenv.cfg"), 5);
    write(&root.join("alpha/env/lib/site.py"), 300);
    write(&root.join("proj/node_modules/x"), 40);
    write(&root.join("home/.cache/pip/w"), 70);
    write(&root.join("loose.txt"), 1);
    let rules = Rules::build(
        &root.join("home"),
        &tmp.path().join("data"),
        None,
        &["**/node_modules".to_string()],
    )
    .unwrap();
    let grants = HashMap::from([(root.join("alpha"), "A".to_string())]);
    Setup {
        _tmp: tmp,
        root,
        rules,
        grants,
    }
}

fn walker<'a>(
    s: &'a Setup,
    backend: &'a dyn Backend,
    units: &'a dyn UnitStore,
    stop: &'a AtomicBool,
    progress: &'a Progress,
    stats: Option<&'a FileStats>,
) -> Walker<'a> {
    Walker {
        backend,
        rules: &s.rules,
        grant_dirs: &s.grants,
        prev: None,
        fold_bytes: 1 << 20,
        fold_files: 100,
        unit_depth: 2,
        units,
        stop,
        progress,
        file_stats: stats,
        root_name: s.root.to_string_lossy().into_owned(),
    }
}

use crate::fs::Backend;

fn by_path(rows: &[DirRow]) -> HashMap<PathBuf, DirRow> {
    rows.iter().map(|r| (r.path.clone(), r.clone())).collect()
}

#[test]
fn walks_folds_and_excludes() {
    let s = setup();
    let (stop, progress, units, stats) = (
        AtomicBool::new(false),
        Progress::default(),
        MemoryUnits::default(),
        FileStats::default(),
    );
    let w = walker(&s, &PosixBackend, &units, &stop, &progress, Some(&stats));
    let (root_row, mut rows) = w.walk_root(&s.root).unwrap();
    rows.extend(units.rows());
    rows.push(root_row.clone());
    let rows = by_path(&rows);
    let r = |p: &str| rows.get(&s.root.join(p)).cloned();

    assert_eq!(root_row.role, Some(Role::Root));
    let total = (2 << 20) + 10 + 5 + 300 + 40 + 70 + 1;
    assert_eq!(root_row.agg.bytes, total);

    let alpha = r("alpha").unwrap();
    assert_eq!(alpha.role, Some(Role::Grant));
    // small/ was folded into alpha's direct totals; big/ has its own row.
    assert_eq!((alpha.direct_bytes, alpha.direct_files), (10, 1));
    assert!(r("alpha/small").is_none());
    assert_eq!(r("alpha/big").unwrap().agg.bytes, 2 << 20);

    let env = r("alpha/env").unwrap();
    assert_eq!(env.excluded, Some(ExcludeKind::Environment));
    assert_eq!(env.agg.bytes, 305);
    assert!(
        env.agg.months.is_empty(),
        "excluded directories aren't walked"
    );
    // ...but their newest change still counts as recent activity, so a parent
    // holding a busy environment isn't reported as stale.
    assert!(env.agg.newest_mtime.is_some());
    assert_eq!(env.agg.newest_mtime, env.agg.newest_change);
    assert_eq!(
        r("proj/node_modules").unwrap().excluded,
        Some(ExcludeKind::User)
    );
    assert_eq!(r("home/.cache").unwrap().excluded, Some(ExcludeKind::Cache));
    assert!(r("alpha/env/lib").is_none());

    // File statistics were split by grant.
    let scopes = stats.scopes.lock().unwrap();
    let grant = &scopes[&("grant", "A".to_string())];
    assert_eq!(grant.extensions["bin"].bytes, 2 << 20);
    assert_eq!(grant.extensions["txt"].files, 1);
    assert!(
        !grant.extensions.contains_key("py"),
        "the venv wasn't walked"
    );
    let largest: Vec<u64> = grant.largest.iter().map(|r| r.0.0).collect();
    assert!(largest.contains(&(2 << 20)));
}

#[test]
fn interrupted_walk_resumes_without_redoing_units() {
    let s = setup();
    let fresh = {
        let (stop, progress, units) = (
            AtomicBool::new(false),
            Progress::default(),
            MemoryUnits::default(),
        );
        let (root, _) = walker(&s, &PosixBackend, &units, &stop, &progress, None)
            .walk_root(&s.root)
            .unwrap();
        (root, units.commits.load(Ordering::SeqCst))
    };
    let (fresh_root, unit_count) = fresh;
    assert!(unit_count >= 3, "expected several units, got {unit_count}");

    static STOP: AtomicBool = AtomicBool::new(false);
    let units = MemoryUnits {
        stop_after: Some((1, &STOP)),
        ..MemoryUnits::default()
    };
    let progress = Progress::default();
    // With a single thread the walk stops right after the first unit.
    let pool = rayon::ThreadPoolBuilder::new()
        .num_threads(1)
        .build()
        .unwrap();
    let first = pool
        .install(|| walker(&s, &PosixBackend, &units, &STOP, &progress, None).walk_root(&s.root));
    assert!(matches!(first, Err(Stop::Interrupted)));
    let after_first = units.commits.load(Ordering::SeqCst);
    assert!(after_first >= 1 && after_first < unit_count);

    STOP.store(false, Ordering::SeqCst);
    let units = MemoryUnits {
        done: Mutex::new(units.done.into_inner().unwrap()),
        ..MemoryUnits::default()
    };
    let (root, _) = walker(&s, &PosixBackend, &units, &STOP, &progress, None)
        .walk_root(&s.root)
        .unwrap();
    assert_eq!(root.agg, fresh_root.agg);
    assert_eq!(
        units.commits.load(Ordering::SeqCst),
        unit_count - after_first
    );
}

struct FakePrev(HashMap<PathBuf, DirRow>);

impl PrevRows for FakePrev {
    fn get(&self, path: &Path) -> Option<DirRow> {
        self.0.get(path).cloned()
    }
}

#[test]
fn incremental_walk_reuses_unchanged_ceph_subtrees() {
    let tmp = tempfile::tempdir().unwrap();
    let root = fs::canonicalize(tmp.path()).unwrap();
    write(&root.join("old/f"), 10);
    write(&root.join("new/g"), 20);
    let fake = FakeXattrs::default();
    fake.set_dir(&root.join("old"), 10, 1, 1_000); // unchanged since long ago
    fake.set_dir(&root.join("new"), 20, 1, 9_999_999_999);
    let backend = CephBackend::new(fake);

    // The previous scan's row for old/ holds different numbers, proving it was reused.
    let mut prev_agg = hist::Agg::default();
    prev_agg.add_file(5_000, 500, 500);
    let prev = FakePrev(HashMap::from([(
        root.join("old"),
        DirRow::new(&root.join("old"), None, None, 5_000, 1, prev_agg),
    )]));
    let rules = Rules::build(&root.join("nohome"), &root.join("nodata"), None, &[]).unwrap();
    let grants = HashMap::new();
    let (stop, progress, units) = (
        AtomicBool::new(false),
        Progress::default(),
        MemoryUnits::default(),
    );
    let w = Walker {
        backend: &backend,
        rules: &rules,
        grant_dirs: &grants,
        prev: Some((&prev, 2_000)),
        fold_bytes: 0, // keep every row
        fold_files: 0,
        unit_depth: 5,
        units: &units,
        stop: &stop,
        progress: &progress,
        file_stats: None,
        root_name: String::new(),
    };
    let (root_row, rows) = w.walk_root(&root).unwrap();
    let rows = by_path(&rows);
    let old = &rows[&root.join("old")];
    assert!(old.reused);
    assert_eq!(old.agg.bytes, 5_000);
    assert!(!rows[&root.join("new")].reused);
    assert_eq!(root_row.agg.bytes, 5_000 + 20);
    assert_eq!(progress.reused.load(Ordering::Relaxed), 1);
}

#[test]
fn full_scan_imports_into_the_catalog() {
    let tmp = tempfile::tempdir().unwrap();
    let base = fs::canonicalize(tmp.path()).unwrap();
    let root = base.join("root");
    write(&root.join("alpha/big/data.bin"), 2 << 20);
    write(&root.join("alpha/small.txt"), 10);
    write(&root.join("other/x.dat"), 3 << 20);
    let data = base.join("data");
    let text = format!(
        "general.roots = [\"{r}\"]\n[[grant]]\nid = \"A\"\ndirs = [\"{r}/alpha\"]\n",
        r = root.display()
    );
    let cfg = Config::parse(&text, Path::new("t.toml")).unwrap();
    let request = Request {
        full: false,
        resume: false,
        task_id: None,
    };
    let (stop, progress) = (AtomicBool::new(false), Progress::default());
    let outcome = run(&cfg, &data, &request, &stop, &progress).unwrap();
    let Outcome::Complete(summary) = outcome else {
        panic!("scan did not complete")
    };
    assert!(summary.full, "the first scan is always full");
    assert_eq!(summary.files, 3);

    let mut catalog = Catalog::open(&data).unwrap();
    assert!(catalog.ingest(&data).unwrap().is_empty());
    assert!(list_results(&inbox_dir(&data)).unwrap().is_empty());
    // Nothing is left behind, including SQLite's persisted journal.
    assert_eq!(fs::read_dir(inbox_dir(&data)).unwrap().count(), 0);
    let scan = catalog.latest_scan(true).unwrap().unwrap();
    assert!(scan.file_stats);
    let rows = catalog
        .dir_rows(scan.id, Some(&root.join("alpha")))
        .unwrap();
    let paths: Vec<&Path> = rows.iter().map(|r| r.path.as_path()).collect();
    assert_eq!(paths, [root.join("alpha"), root.join("alpha/big")]);
    assert_eq!(rows[0].agg.bytes, (2 << 20) + 10);
    let ext = catalog.scan_extensions(scan.id, "grant", "A").unwrap();
    assert_eq!(ext[0].0, "bin");
    let largest = catalog
        .scan_largest(scan.id, "root", &root.to_string_lossy())
        .unwrap();
    assert_eq!(largest[0].1, 3 << 20);

    // A resume request with nothing to resume starts a new scan.
    let request = Request {
        full: false,
        resume: true,
        task_id: None,
    };
    let progress = Progress::default();
    assert!(matches!(
        run(&cfg, &data, &request, &stop, &progress).unwrap(),
        Outcome::Complete(_)
    ));
    catalog.ingest(&data).unwrap();
    assert_eq!(catalog.scan_count().unwrap(), 2);
}

#[test]
fn interrupted_scan_is_left_in_the_inbox_and_resumed() {
    let tmp = tempfile::tempdir().unwrap();
    let base = fs::canonicalize(tmp.path()).unwrap();
    let root = base.join("root");
    write(&root.join("a/b/c"), 1);
    let data = base.join("data");
    let text = format!("general.roots = [\"{}\"]\n", root.display());
    let cfg = Config::parse(&text, Path::new("t.toml")).unwrap();
    let request = Request {
        full: false,
        resume: false,
        task_id: Some("scan-test".into()),
    };
    let stop = AtomicBool::new(true); // stops before doing anything
    let outcome = run(&cfg, &data, &request, &stop, &Progress::default()).unwrap();
    assert!(matches!(outcome, Outcome::Incomplete(_)));
    let pending = list_results(&inbox_dir(&data)).unwrap();
    assert_eq!(pending.len(), 1);
    assert_eq!(pending[0].meta.status, "incomplete");

    // Importing leaves it alone; the same task resumes it.
    Catalog::open(&data).unwrap().ingest(&data).unwrap();
    assert_eq!(list_results(&inbox_dir(&data)).unwrap().len(), 1);
    stop.store(false, Ordering::SeqCst);
    let Outcome::Complete(summary) =
        run(&cfg, &data, &request, &stop, &Progress::default()).unwrap()
    else {
        panic!("resumed scan did not complete")
    };
    assert!(summary.resumed);
    assert_eq!(summary.uuid, pending[0].meta.uuid);
}
