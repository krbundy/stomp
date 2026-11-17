//! Archive tests: round trips with awkward files, a simulated crash after
//! every journaled step, files that change mid-archive, corrupted chunks,
//! partial restores, and the targets stomp refuses.

use std::collections::{BTreeMap, HashMap};
use std::ffi::OsStr;
use std::fs::{self, File, FileTimes};
use std::os::unix::ffi::OsStrExt;
use std::os::unix::fs::{MetadataExt, PermissionsExt, symlink};
use std::path::{Path, PathBuf};

use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::time::{Duration, UNIX_EPOCH};

use super::journal::Event;
use super::manifest::{self, Record, SkipReason};
use super::*;
use crate::fs::posix::PosixBackend;

const CHUNK: u64 = 64 << 10;

fn bytes(seed: u8, len: usize) -> Vec<u8> {
    // Mildly compressible, deterministic content.
    (0..len)
        .map(|i| (i as u8).wrapping_mul(seed) ^ (i / 7) as u8)
        .collect()
}

fn put(path: &Path, content: &[u8]) {
    fs::create_dir_all(path.parent().unwrap()).unwrap();
    fs::write(path, content).unwrap();
}

fn set_mtime(path: &Path, secs: u64) {
    let f = File::open(path).unwrap();
    f.set_times(FileTimes::new().set_modified(UNIX_EPOCH + Duration::from_secs(secs)))
        .unwrap();
}

/// Builds a tree full of awkward cases, with fixed times so snapshots compare equal.
fn build(source: &Path) {
    put(&source.join("a/one.txt"), &b"hello ".repeat(1000));
    put(&source.join("a/big.bin"), &bytes(31, 200 << 10)); // larger than a chunk
    put(&source.join("a/empty"), b"");
    put(&source.join("a/deep/er/x.dat"), &bytes(7, 30 << 10));
    symlink("one.txt", source.join("a/link")).unwrap();
    symlink("/nonexistent/target", source.join("a/dangling")).unwrap();
    put(&source.join("b/hard1"), &bytes(3, 20 << 10));
    // Fills the chunk, so c/hard2 lands in a later chunk than b/hard1.
    put(&source.join("b/zfill"), &bytes(11, 50 << 10));
    fs::create_dir_all(source.join("c")).unwrap();
    fs::hard_link(source.join("b/hard1"), source.join("c/hard2")).unwrap();
    put(&source.join("b/same1"), b"shared");
    fs::hard_link(source.join("b/same1"), source.join("b/same2")).unwrap(); // same chunk
    put(&source.join("weird\nname.txt"), b"newline in the name");
    put(
        &source.join(OsStr::from_bytes(b"caf\xe9.txt")),
        b"not utf-8",
    );
    let long = "x".repeat(120);
    put(&source.join("long").join(&long).join("f.txt"), b"long path");
    fs::create_dir_all(source.join("emptydir")).unwrap();
    put(&source.join("private.txt"), b"secret");
    fs::set_permissions(
        source.join("private.txt"),
        fs::Permissions::from_mode(0o640),
    )
    .unwrap();
    put(&source.join("restricted/r.txt"), b"r");

    // Fixed times: files first, then directories deepest first.
    let mut dirs = Vec::new();
    for entry in walk(source) {
        let md = fs::symlink_metadata(&entry).unwrap();
        if md.is_dir() {
            dirs.push(entry);
        } else if md.is_file() {
            set_mtime(&entry, 1_600_000_000);
        }
    }
    fs::set_permissions(source.join("restricted"), fs::Permissions::from_mode(0o750)).unwrap();
    dirs.sort_by(|a, b| b.cmp(a));
    for d in dirs {
        set_mtime(&d, 1_500_000_000);
    }
}

fn walk(dir: &Path) -> Vec<PathBuf> {
    let mut out = Vec::new();
    for entry in fs::read_dir(dir).unwrap() {
        let path = entry.unwrap().path();
        let md = fs::symlink_metadata(&path).unwrap();
        out.push(path.clone());
        if md.is_dir() {
            out.extend(walk(&path));
        }
    }
    out
}

#[derive(Debug, PartialEq)]
enum Node {
    File {
        mode: u32,
        mtime: i64,
        content: Vec<u8>,
        group: Option<usize>,
    },
    Dir {
        mode: u32,
        mtime: i64,
    },
    Link {
        target: PathBuf,
    },
}

/// Everything under `dir` except stomp's own files, with hard links grouped.
fn snapshot(dir: &Path) -> BTreeMap<PathBuf, Node> {
    let mut paths: Vec<PathBuf> = walk(dir)
        .into_iter()
        .filter(|p| {
            let rel = p.strip_prefix(dir).unwrap();
            let first = rel.components().next().unwrap().as_os_str();
            first != ARCHIVE_DIR && first != NOTE_FILE
        })
        .collect();
    paths.sort();
    let mut groups: HashMap<u64, usize> = HashMap::new();
    let mut out = BTreeMap::new();
    for p in paths {
        let md = fs::symlink_metadata(&p).unwrap();
        let rel = p.strip_prefix(dir).unwrap().to_path_buf();
        let node = if md.file_type().is_symlink() {
            Node::Link {
                target: fs::read_link(&p).unwrap(),
            }
        } else if md.is_dir() {
            Node::Dir {
                mode: md.mode() & 0o7777,
                mtime: md.mtime(),
            }
        } else {
            let group = (md.nlink() > 1).then(|| {
                let n = groups.len();
                *groups.entry(md.ino()).or_insert(n)
            });
            Node::File {
                mode: md.mode() & 0o7777,
                mtime: md.mtime(),
                content: fs::read(&p).unwrap(),
                group,
            }
        };
        out.insert(rel, node);
    }
    out
}

/// A destination that is just a local directory, with failures on demand.
pub(crate) struct DirDest {
    root: PathBuf,
    /// Paths whose next upload fails.
    fail_next: std::sync::Mutex<std::collections::HashSet<String>>,
}

impl DirDest {
    fn new(root: PathBuf) -> DirDest {
        fs::create_dir_all(&root).unwrap();
        DirDest {
            root,
            fail_next: Default::default(),
        }
    }

    fn files(&self) -> Vec<String> {
        let mut out: Vec<String> = walk(&self.root)
            .into_iter()
            .filter(|p| p.is_file())
            .map(|p| {
                p.strip_prefix(&self.root)
                    .unwrap()
                    .to_string_lossy()
                    .into_owned()
            })
            .collect();
        out.sort();
        out
    }
}

impl crate::dest::Destination for DirDest {
    fn name(&self) -> &str {
        "store"
    }
    fn kind(&self) -> &'static str {
        "test"
    }
    fn location(&self, rel: &str) -> String {
        format!("store:{rel}")
    }
    fn put(&self, local: &Path, rel: &str, sha256: &str, _: &Path) -> anyhow::Result<()> {
        if self.fail_next.lock().unwrap().remove(rel) {
            anyhow::bail!("upload of {rel} failed (test)");
        }
        let target = self.root.join(rel);
        fs::create_dir_all(target.parent().unwrap())?;
        fs::copy(local, &target)?;
        anyhow::ensure!(manifest::sha256_file(&target)? == sha256, "bad copy");
        Ok(())
    }
    fn verify(&self, rel: &str, sha256: &str, _: u64, _: &Path) -> anyhow::Result<()> {
        anyhow::ensure!(
            manifest::sha256_file(&self.root.join(rel))? == sha256,
            "mismatch"
        );
        Ok(())
    }
    fn get(&self, rel: &str, local: &Path) -> anyhow::Result<()> {
        fs::copy(self.root.join(rel), local)?;
        Ok(())
    }
    fn delete(&self, rel: &str) -> anyhow::Result<()> {
        let _ = fs::remove_file(self.root.join(rel));
        Ok(())
    }
    fn delete_dir(&self, rel: &str) -> anyhow::Result<()> {
        let _ = fs::remove_dir(self.root.join(rel));
        Ok(())
    }
    fn list(&self, _: &str) -> anyhow::Result<Vec<(String, u64)>> {
        Ok(Vec::new())
    }
    fn check(&self) -> Vec<crate::dest::Check> {
        Vec::new()
    }
}

struct Fixture {
    _tmp: tempfile::TempDir,
    base: PathBuf,
    source: PathBuf,
    remote: Option<DirDest>,
}

impl Fixture {
    fn new() -> Fixture {
        let tmp = tempfile::tempdir().unwrap();
        let base = fs::canonicalize(tmp.path()).unwrap();
        let source = base.join("root/proj");
        build(&source);
        Fixture {
            _tmp: tmp,
            base,
            source,
            remote: None,
        }
    }

    /// A fixture whose archives go to a remote destination.
    fn remote() -> Fixture {
        let mut f = Fixture::new();
        f.remote = Some(DirDest::new(f.base.join("remote")));
        f
    }

    fn dest(&self) -> Option<&dyn crate::dest::Destination> {
        self.remote
            .as_ref()
            .map(|d| d as &dyn crate::dest::Destination)
    }

    fn data(&self) -> PathBuf {
        self.base.join("data")
    }

    fn settings(&self) -> Settings {
        Settings {
            id: "T-20261117-000001".into(),
            source: self.source.clone(),
            grant: Some("T".into()),
            chunk_size: CHUNK,
            level: 3,
            threads: 1,
            only_owned: true,
            destination: if self.remote.is_some() {
                "store"
            } else {
                "local"
            }
            .into(),
            staging: self.remote.as_ref().map(|_| self.base.join("staging")),
        }
    }

    fn archive_with(&self, hook: Hook) -> Result<Outcome> {
        let rules = Rules::build(
            &self.base.join("home"),
            &self.data(),
            None,
            &["**/node_modules".to_string()],
        )?;
        archive(
            &self.settings(),
            &rules,
            &PosixBackend,
            self.dest(),
            &self.data(),
            &AtomicBool::new(false),
            &Progress::default(),
            hook,
        )
    }

    fn archive(&self) -> Summary {
        match self.archive_with(&|_| false).unwrap() {
            Outcome::Complete(s) => s,
            Outcome::Incomplete(_) => panic!("archive didn't finish"),
        }
    }

    fn archive_dir(&self) -> PathBuf {
        archive_path(&self.source, "T-20261117-000001")
    }

    fn restore(&self, options: &RestoreOptions) -> RestoreSummary {
        match restore(
            &self.archive_dir(),
            &self.source,
            options,
            self.dest(),
            &self.data(),
            &AtomicBool::new(false),
        )
        .unwrap()
        {
            RestoreOutcome::Complete(s) => s,
            RestoreOutcome::Incomplete(_) => panic!("restore didn't finish"),
        }
    }

    /// What's left in the source besides stomp's own files.
    fn leftovers(&self) -> Vec<PathBuf> {
        snapshot(&self.source).into_keys().collect()
    }
}

#[test]
fn round_trip_restores_everything() {
    let f = Fixture::new();
    let before = snapshot(&f.source);
    let summary = f.archive();
    assert!(
        summary.chunks >= 3,
        "expected several chunks, got {}",
        summary.chunks
    );
    assert_eq!(summary.kept, 0);

    // Only empty-of-files directories could remain, and they were removed too.
    assert!(f.leftovers().is_empty(), "left behind: {:?}", f.leftovers());
    assert!(f.source.join(NOTE_FILE).exists());
    let dir = f.archive_dir();
    for name in [manifest::MANIFEST, manifest::CHECKSUMS, manifest::README] {
        assert!(dir.join(name).exists(), "{name} missing");
    }
    assert!(!dir.join("plan").exists() && !dir.join("manifest.d").exists());

    // The manifest header's totals, and checksums that `sha256sum -c` would accept.
    let records = manifest::read_records(&dir.join(manifest::MANIFEST)).unwrap();
    let Record::Header(header) = &records[0] else {
        panic!("no header first")
    };
    assert_eq!(header.totals.hardlinks, 2);
    assert_eq!(header.totals.symlinks, 2);
    let chunk_of = |name: &str| {
        records
            .iter()
            .find_map(|r| match r {
                Record::Entry(e) if e.rel().unwrap() == Path::new(name) => Some(e.chunk),
                _ => None,
            })
            .unwrap()
    };
    assert_ne!(
        chunk_of("b/hard1"),
        chunk_of("c/hard2"),
        "test needs a hard link across chunks"
    );
    assert_eq!(chunk_of("b/same1"), chunk_of("b/same2"));
    for (name, sum) in manifest::read_checksums(&dir.join(manifest::CHECKSUMS)).unwrap() {
        assert_eq!(
            manifest::sha256_file(&dir.join(&name)).unwrap(),
            sum,
            "{name}"
        );
    }

    let restored = f.restore(&RestoreOptions::default());
    assert!(restored.conflicts.is_empty(), "{:?}", restored.conflicts);
    assert!(restored.archive_removed);
    assert_eq!(snapshot(&f.source), before);
    assert!(!f.source.join(NOTE_FILE).exists());
    assert!(!f.source.join(ARCHIVE_DIR).exists());
}

#[test]
fn leaves_environments_excluded_and_special_files_in_place() {
    let f = Fixture::new();
    put(&f.source.join("env/pyvenv.cfg"), b"home = /usr");
    put(&f.source.join("env/lib/site.py"), b"x");
    put(&f.source.join("web/node_modules/pkg.js"), b"y");
    rustix::fs::mknodat(
        rustix::fs::CWD,
        f.source.join("pipe"),
        rustix::fs::FileType::Fifo,
        rustix::fs::Mode::from_raw_mode(0o600),
        0,
    )
    .unwrap();
    f.archive();
    for kept in [
        "env/pyvenv.cfg",
        "env/lib/site.py",
        "web/node_modules/pkg.js",
        "pipe",
    ] {
        assert!(
            fs::symlink_metadata(f.source.join(kept)).is_ok(),
            "{kept} should remain"
        );
    }
    let reasons: Vec<SkipReason> =
        manifest::read_records(&f.archive_dir().join(manifest::MANIFEST))
            .unwrap()
            .into_iter()
            .filter_map(|r| match r {
                Record::Skipped(s) => Some(s.reason),
                _ => None,
            })
            .collect();
    for reason in [
        SkipReason::Environment,
        SkipReason::Excluded,
        SkipReason::SpecialFile,
    ] {
        assert!(
            reasons.contains(&reason),
            "{reason:?} not recorded in {reasons:?}"
        );
    }
}

#[test]
fn every_crash_point_resumes_without_losing_data() {
    crash_everywhere(Fixture::new);
}

#[test]
fn every_crash_point_of_a_remote_archive_resumes_without_losing_data() {
    crash_everywhere(Fixture::remote);
}

/// Stops an archive after each journaled step in turn, resumes it, restores
/// it, and checks that every file came back exactly.
fn crash_everywhere(make: fn() -> Fixture) {
    // Count the journaled steps of an uninterrupted run.
    let steps = {
        let f = make();
        let count = AtomicUsize::new(0);
        f.archive_with(&|_| {
            count.fetch_add(1, Ordering::SeqCst);
            false
        })
        .unwrap();
        count.load(Ordering::SeqCst)
    };
    assert!(steps >= 10, "expected many steps, got {steps}");

    for stop_at in 1..=steps {
        let f = make();
        let before = snapshot(&f.source);
        let count = AtomicUsize::new(0);
        let first = f
            .archive_with(&|_| count.fetch_add(1, Ordering::SeqCst) + 1 == stop_at)
            .unwrap();
        assert!(
            matches!(first, Outcome::Incomplete(_)),
            "step {stop_at} didn't stop"
        );
        // The interrupted state can always be restored from directly...
        if stop_at % 3 == 0 {
            let options = RestoreOptions {
                to: Some(f.base.join(format!("rescue-{stop_at}"))),
                ..RestoreOptions::default()
            };
            let _ = restore(
                &f.archive_dir(),
                &f.source,
                &options,
                f.dest(),
                &f.data(),
                &AtomicBool::new(false),
            );
        }
        // ...and resuming finishes the job.
        let second = f.archive_with(&|_| false).unwrap();
        assert!(
            matches!(second, Outcome::Complete(_)),
            "step {stop_at} didn't resume"
        );
        assert!(
            f.leftovers().is_empty(),
            "step {stop_at}: left {:?}",
            f.leftovers()
        );
        f.restore(&RestoreOptions::default());
        assert_eq!(
            snapshot(&f.source),
            before,
            "data differs after crash at step {stop_at}"
        );
    }
}

#[test]
fn a_file_changed_after_archiving_is_kept() {
    let f = Fixture::new();
    // Stop once the first chunk is verified, edit a file in it, then resume.
    let first = f
        .archive_with(&|e| matches!(e, Event::Verified { chunk: 1 }))
        .unwrap();
    assert!(matches!(first, Outcome::Incomplete(_)));
    let records = entry_records(&f.archive_dir().join("manifest.d/chunk-0001.jsonl")).unwrap();
    let victim = records
        .iter()
        .find(|r| r.kind == manifest::Kind::File && r.size > 0)
        .unwrap()
        .rel()
        .unwrap();
    fs::write(f.source.join(&victim), b"edited after archiving").unwrap();

    let summary = f.archive();
    assert_eq!(summary.kept, 1);
    assert_eq!(
        fs::read(f.source.join(&victim)).unwrap(),
        b"edited after archiving"
    );
    let kept: Vec<_> = manifest::read_records(&f.archive_dir().join(manifest::MANIFEST))
        .unwrap()
        .into_iter()
        .filter_map(|r| match r {
            Record::Entry(e) if e.kept => Some(e.rel().unwrap()),
            _ => None,
        })
        .collect();
    assert_eq!(kept, std::slice::from_ref(&victim));

    // Restoring leaves the newer file alone and still counts as complete.
    let restored = f.restore(&RestoreOptions::default());
    assert!(restored.conflicts.is_empty());
    assert_eq!(
        fs::read(f.source.join(&victim)).unwrap(),
        b"edited after archiving"
    );
}

#[test]
fn a_corrupted_chunk_is_discarded_and_nothing_is_deleted() {
    let f = Fixture::new();
    let before = snapshot(&f.source);
    f.archive_with(&|e| matches!(e, Event::Written { chunk: 1, .. }))
        .unwrap();
    let chunk = f.archive_dir().join(manifest::chunk_name(1));
    let mut data = fs::read(&chunk).unwrap();
    let middle = data.len() / 2;
    data[middle] ^= 0xff;
    fs::write(&chunk, data).unwrap();

    let err = f
        .archive_with(&|_| false)
        .err()
        .expect("verification should fail");
    assert!(
        format!("{err:#}").contains("failed verification"),
        "{err:#}"
    );
    assert_eq!(snapshot(&f.source), before, "nothing may be deleted");
    assert!(!chunk.exists(), "the bad chunk is discarded");

    // The next run rewrites the chunk and finishes.
    f.archive();
    f.restore(&RestoreOptions::default());
    assert_eq!(snapshot(&f.source), before);
}

#[test]
fn partial_restore_elsewhere_keeps_the_archive() {
    let f = Fixture::new();
    f.archive();
    let out = f.base.join("out");
    let restored = f.restore(&RestoreOptions {
        only: vec!["a/deep".into(), "b/*".into()],
        to: Some(out.clone()),
        ..RestoreOptions::default()
    });
    assert!(!restored.archive_removed);
    assert_eq!(fs::read(out.join("b/same1")).unwrap(), b"shared");
    assert_eq!(
        fs::read(out.join("a/deep/er/x.dat")).unwrap(),
        bytes(7, 30 << 10),
    );
    assert!(!out.join("a/one.txt").exists());
    assert!(f.archive_dir().join(manifest::MANIFEST).exists());
}

#[test]
fn restore_refuses_to_overwrite_new_files() {
    let f = Fixture::new();
    f.archive();
    put(
        &f.source.join("a/one.txt"),
        b"a new file with the same name",
    );
    let restored = f.restore(&RestoreOptions::default());
    assert_eq!(restored.conflicts, ["a/one.txt"]);
    assert!(
        !restored.archive_removed,
        "the archive must be kept when something wasn't restored"
    );
    assert_eq!(
        fs::read(f.source.join("a/one.txt")).unwrap(),
        b"a new file with the same name"
    );
}

#[test]
fn refuses_unsafe_targets() {
    let tmp = tempfile::tempdir().unwrap();
    let base = fs::canonicalize(tmp.path()).unwrap();
    let root = base.join("root");
    for d in ["grant/run", "mixed/grant2", "venv", "done"] {
        fs::create_dir_all(root.join(d)).unwrap();
    }
    put(&root.join("venv/pyvenv.cfg"), b"");
    put(&root.join("done").join(NOTE_FILE), b"");
    let text = format!(
        "general.roots = [\"{r}\"]\n[[grant]]\nid = \"A\"\ndirs = [\"{r}/grant\"]\n[[grant]]\nid = \"B\"\ndirs = [\"{r}/mixed/grant2\"]\n",
        r = root.display()
    );
    let cfg = crate::config::Config::parse(&text, Path::new("t.toml")).unwrap();
    let data = base.join("data");
    let check =
        |p: &str| check_target(&cfg, &root.join(p), &data, false).map_err(|e| e.to_string());
    assert!(check("grant/run").is_ok());
    assert!(check("grant").is_ok());
    assert!(check("mixed").unwrap_err().contains("grant(s) B"));
    assert!(check("venv").unwrap_err().contains("software environment"));
    assert!(check("done").unwrap_err().contains("already archived"));
    assert!(
        check_target(&cfg, &root, &data, false)
            .unwrap_err()
            .to_string()
            .contains("is a root")
    );
}

#[test]
fn estimate_reflects_compressibility() {
    let f = Fixture::new();
    let rules = Rules::build(&f.base.join("home"), &f.data(), None, &[]).unwrap();
    let plan = plan::make_plan(&f.source, &rules, &PosixBackend, true, CHUNK).unwrap();
    let e = estimate(&f.source, &plan, 3, 2).unwrap();
    assert!(e.sampled_files > 0);
    assert!(e.ratio < 0.9, "test data compresses, got {}", e.ratio);
    assert_eq!(e.files_after, u64::from(plan.summary.chunks) + 6);
}

#[test]
fn remote_archive_round_trip() {
    let f = Fixture::remote();
    let before = snapshot(&f.source);
    let summary = f.archive();
    let remote = f.remote.as_ref().unwrap();
    let files = remote.files();
    let chunks = files.iter().filter(|p| p.contains("chunk-")).count();
    assert_eq!(chunks as u32, summary.chunks);
    for name in [manifest::MANIFEST, manifest::CHECKSUMS, manifest::README] {
        assert!(
            files.contains(&format!("T/T-20261117-000001/{name}")),
            "{name} not uploaded: {files:?}"
        );
    }
    // Only the records stay local; no chunk is left on disk or in staging.
    let dir = f.archive_dir();
    assert!(dir.join(manifest::MANIFEST).exists());
    assert!(
        !walk(&dir)
            .iter()
            .any(|p| p.to_string_lossy().contains("chunk-"))
    );
    assert!(!walk(&f.base.join("staging")).iter().any(|p| p.is_file()));
    assert!(
        fs::read_to_string(f.source.join(NOTE_FILE))
            .unwrap()
            .contains("store:T/T-20261117-000001")
    );
    let readme = fs::read_to_string(dir.join(manifest::README)).unwrap();
    assert!(
        readme.contains("Download every file from store:"),
        "{readme}"
    );

    // Restoring downloads the chunks; the remote copy stays unless asked.
    let restored = f.restore(&RestoreOptions::default());
    assert!(restored.archive_removed);
    assert_eq!(snapshot(&f.source), before);
    assert_eq!(remote.files().len(), files.len(), "the remote copy is kept");
}

#[test]
fn restore_can_delete_the_remote_copy() {
    let f = Fixture::remote();
    f.archive();
    f.restore(&RestoreOptions {
        delete_remote: true,
        ..RestoreOptions::default()
    });
    assert!(f.remote.as_ref().unwrap().files().is_empty());
}

#[test]
fn a_failed_upload_deletes_nothing() {
    let f = Fixture::remote();
    let before = snapshot(&f.source);
    f.remote
        .as_ref()
        .unwrap()
        .fail_next
        .lock()
        .unwrap()
        .insert("T/T-20261117-000001/chunk-0001.tar.zst".into());
    assert!(f.archive_with(&|_| false).is_err());
    assert_eq!(
        snapshot(&f.source),
        before,
        "nothing may be deleted before the upload is verified"
    );
    f.archive();
    f.restore(&RestoreOptions::default());
    assert_eq!(snapshot(&f.source), before);
}

#[test]
fn archives_move_between_local_and_remote() {
    let mut f = Fixture::new();
    let before = snapshot(&f.source);
    f.archive();
    let remote = DirDest::new(f.base.join("remote"));
    let dir = f.archive_dir();
    let stop = AtomicBool::new(false);

    // Out to the remote destination...
    let out = relocate(&dir, None, Some(&remote), &f.data(), &stop).unwrap();
    assert!(matches!(out, RelocateOutcome::Complete(at) if at == "store:T/T-20261117-000001"));
    assert!(
        !walk(&dir)
            .iter()
            .any(|p| p.to_string_lossy().contains("chunk-"))
    );
    assert!(
        remote
            .files()
            .iter()
            .any(|p| p.ends_with("chunk-0001.tar.zst"))
    );
    assert_eq!(load_settings(&dir).unwrap().destination, "store");
    assert!(
        relocate(&dir, Some(&remote), Some(&remote), &f.data(), &stop).is_err(),
        "already there"
    );

    // ...and back again.
    relocate(&dir, Some(&remote), None, &f.data(), &stop).unwrap();
    assert!(
        remote.files().is_empty(),
        "the remote copy is removed after the move"
    );
    assert!(dir.join(manifest::chunk_name(1)).exists());
    assert_eq!(load_settings(&dir).unwrap().destination, "local");

    // Out once more, then restore straight from the remote.
    relocate(&dir, None, Some(&remote), &f.data(), &stop).unwrap();
    f.remote = Some(remote);
    f.restore(&RestoreOptions::default());
    assert_eq!(snapshot(&f.source), before);
}
