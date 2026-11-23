//! Archive file formats (§12.2): manifest records, plan files, checksums,
//! and the plain-text notes left for people.
//!
//! Records are JSON lines, compressed with zstd where they can be large.
//! Paths are stored relative to the archived directory, as text when they're
//! valid UTF-8 and base64 otherwise.

use std::fs::{self, File};
use std::io::{self, BufRead, BufReader, BufWriter, Read, Write};
use std::os::unix::ffi::OsStrExt;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result, bail};
use base64::Engine;
use base64::engine::general_purpose::STANDARD as BASE64;
use serde::de::DeserializeOwned;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::units::{format_bytes, format_count};

pub const FORMAT: &str = "stomp-archive";
pub const VERSION: u32 = 1;
pub const MANIFEST: &str = "manifest.jsonl.zst";
pub const CHECKSUMS: &str = "SHA256SUMS";
pub const README: &str = "README.txt";

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Kind {
    File,
    Dir,
    Symlink,
    Hardlink,
}

/// Why something was left in place instead of archived.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum SkipReason {
    Environment,
    Cache,
    Excluded,
    StompData,
    NestedArchive,
    NotOwned,
    SpecialFile,
    OtherFilesystem,
    Unreadable,
    Vanished,
    ChangedType,
}

impl SkipReason {
    pub fn label(self) -> &'static str {
        match self {
            Self::Environment => "software environments",
            Self::Cache => "caches",
            Self::Excluded => "excluded by scan.exclude",
            Self::StompData => "stomp's own data",
            Self::NestedArchive => "earlier stomp archives",
            Self::NotOwned => "owned by other users",
            Self::SpecialFile => "sockets, pipes and devices",
            Self::OtherFilesystem => "other filesystems mounted inside",
            Self::Unreadable => "unreadable",
            Self::Vanished => "deleted while archiving",
            Self::ChangedType => "replaced by a different kind of file while archiving",
        }
    }
}

/// A relative path: `path` when it's valid UTF-8, `path_b64` otherwise.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct PathField {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub path: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub path_b64: Option<String>,
}

/// A link target, encoded like [`PathField`].
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct TargetField {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub target: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub target_b64: Option<String>,
}

fn encode(p: &Path) -> (Option<String>, Option<String>) {
    match p.to_str() {
        Some(s) => (Some(s.to_string()), None),
        None => (None, Some(BASE64.encode(p.as_os_str().as_bytes()))),
    }
}

fn decode(text: &Option<String>, b64: &Option<String>) -> Result<PathBuf> {
    match (text, b64) {
        (Some(t), _) => Ok(PathBuf::from(t)),
        (None, Some(b)) => Ok(PathBuf::from(std::ffi::OsStr::from_bytes(
            &BASE64.decode(b)?,
        ))),
        (None, None) => bail!("record has no path"),
    }
}

impl PathField {
    pub fn new(p: &Path) -> Self {
        let (path, path_b64) = encode(p);
        PathField { path, path_b64 }
    }

    pub fn get(&self) -> Result<PathBuf> {
        decode(&self.path, &self.path_b64)
    }
}

impl TargetField {
    pub fn new(p: Option<&Path>) -> Self {
        match p {
            Some(p) => {
                let (target, target_b64) = encode(p);
                TargetField { target, target_b64 }
            }
            None => TargetField::default(),
        }
    }

    pub fn get(&self) -> Result<Option<PathBuf>> {
        if self.target.is_none() && self.target_b64.is_none() {
            return Ok(None);
        }
        decode(&self.target, &self.target_b64).map(Some)
    }
}

fn is_false(b: &bool) -> bool {
    !*b
}

fn entry_tag() -> String {
    "entry".to_string()
}

fn skipped_tag() -> String {
    "skipped".to_string()
}

fn header_tag() -> String {
    "header".to_string()
}

/// One archived entry.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EntryRecord {
    #[serde(rename = "type", default = "entry_tag")]
    pub record: String,
    #[serde(flatten)]
    pub path: PathField,
    pub kind: Kind,
    pub chunk: u32,
    pub size: u64,
    pub mode: u32,
    pub mtime: i64,
    pub uid: u32,
    pub gid: u32,
    /// BLAKE3 of the content, for files.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub blake3: Option<String>,
    /// For symlinks, what they point to; for hard links, the archived path they share content with.
    #[serde(flatten)]
    pub target: TargetField,
    /// The file changed while it was being read, so the archived copy may be
    /// inconsistent; the original was left in place.
    #[serde(default, skip_serializing_if = "is_false")]
    pub changed: bool,
    /// The original was left in place: it changed after being archived, or couldn't be removed.
    #[serde(default, skip_serializing_if = "is_false")]
    pub kept: bool,
    /// Identity of the original when archived, checked before deleting it (§12.4, I2).
    #[serde(default)]
    pub ino: u64,
    #[serde(default)]
    pub mtime_ns: i64,
    #[serde(default)]
    pub ctime_ns: i64,
}

impl EntryRecord {
    pub fn rel(&self) -> Result<PathBuf> {
        self.path.get()
    }
}

/// Something left in place.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SkipRecord {
    #[serde(rename = "type", default = "skipped_tag")]
    pub record: String,
    #[serde(flatten)]
    pub path: PathField,
    pub reason: SkipReason,
    pub bytes: u64,
}

impl SkipRecord {
    pub fn new(rel: &Path, reason: SkipReason, bytes: u64) -> Self {
        SkipRecord {
            record: skipped_tag(),
            path: PathField::new(rel),
            reason,
            bytes,
        }
    }
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct Totals {
    pub files: u64,
    pub bytes: u64,
    pub dirs: u64,
    pub symlinks: u64,
    pub hardlinks: u64,
    pub chunks: u32,
    pub compressed_bytes: u64,
    /// Originals left in place (changed while archiving, or not removable).
    pub kept: u64,
    pub skipped: u64,
    pub skipped_bytes: u64,
}

/// The first record of a finished manifest.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Header {
    #[serde(rename = "type", default = "header_tag")]
    pub record: String,
    pub format: String,
    pub version: u32,
    pub archive_id: String,
    pub source: String,
    pub grant: Option<String>,
    pub user: Option<String>,
    pub host: String,
    pub stomp_version: String,
    pub created_at: String,
    pub finished_at: String,
    pub zstd_level: i32,
    pub chunk_size: u64,
    pub totals: Totals,
    /// Where the chunks were stored when the archive was made (`local` or a destination).
    #[serde(default)]
    pub destination: Option<String>,
    /// For a remote archive, where its files are.
    #[serde(default)]
    pub location: Option<String>,
}

pub enum Record {
    Header(Box<Header>),
    Entry(EntryRecord),
    Skipped(SkipRecord),
}

/// Parses one manifest line.
pub fn parse_record(line: &str) -> Result<Record> {
    let value: serde_json::Value = serde_json::from_str(line)?;
    Ok(match value.get("type").and_then(|t| t.as_str()) {
        Some("header") => Record::Header(Box::new(serde_json::from_value(value)?)),
        Some("entry") => Record::Entry(serde_json::from_value(value)?),
        Some("skipped") => Record::Skipped(serde_json::from_value(value)?),
        other => bail!("unknown manifest record type {other:?}"),
    })
}

// ---------------------------------------------------------------------------
// Files
// ---------------------------------------------------------------------------

/// Flushes a directory so renames inside it survive a crash.
pub fn sync_dir(dir: &Path) -> io::Result<()> {
    File::open(dir)?.sync_all()
}

/// Writes a file atomically: to a temporary name, synced, then renamed.
pub fn write_atomic(path: &Path, write: impl FnOnce(&mut dyn Write) -> Result<()>) -> Result<()> {
    let tmp = tmp_path(path);
    {
        let mut out = BufWriter::new(File::create(&tmp)?);
        write(&mut out)?;
        out.into_inner().map_err(|e| e.into_error())?.sync_all()?;
    }
    fs::rename(&tmp, path)?;
    if let Some(parent) = path.parent() {
        sync_dir(parent)?;
    }
    Ok(())
}

fn tmp_path(path: &Path) -> PathBuf {
    let mut name = path.file_name().unwrap_or_default().to_owned();
    name.push(".tmp");
    path.with_file_name(name)
}

/// Writes JSON lines, zstd-compressed when the name ends in `.zst`.
pub fn write_jsonl<T: Serialize>(path: &Path, items: impl IntoIterator<Item = T>) -> Result<()> {
    let compress = path.extension().is_some_and(|x| x == "zst");
    write_atomic(path, |out| {
        if compress {
            let mut enc = zstd::Encoder::new(out, 9)?;
            for item in items {
                serde_json::to_writer(&mut enc, &item)?;
                enc.write_all(b"\n")?;
            }
            enc.finish()?;
        } else {
            for item in items {
                serde_json::to_writer(&mut *out, &item)?;
                out.write_all(b"\n")?;
            }
        }
        Ok(())
    })
}

fn open_lines(path: &Path) -> Result<Box<dyn BufRead>> {
    let file = File::open(path).with_context(|| format!("cannot open {}", path.display()))?;
    Ok(if path.extension().is_some_and(|x| x == "zst") {
        Box::new(BufReader::new(zstd::Decoder::new(file)?))
    } else {
        Box::new(BufReader::new(file))
    })
}

/// Reads JSON lines written by [`write_jsonl`].
pub fn read_jsonl<T: DeserializeOwned>(path: &Path) -> Result<Vec<T>> {
    let mut out = Vec::new();
    for line in open_lines(path)?.lines() {
        let line = line?;
        if !line.trim().is_empty() {
            out.push(
                serde_json::from_str(&line).with_context(|| format!("in {}", path.display()))?,
            );
        }
    }
    Ok(out)
}

/// Reads manifest records from a final manifest or a per-chunk file.
pub fn read_records(path: &Path) -> Result<Vec<Record>> {
    let mut out = Vec::new();
    for line in open_lines(path)?.lines() {
        let line = line?;
        if !line.trim().is_empty() {
            out.push(parse_record(&line).with_context(|| format!("in {}", path.display()))?);
        }
    }
    Ok(out)
}

/// SHA-256 of a file, as hex.
pub fn sha256_file(path: &Path) -> Result<String> {
    let mut file = File::open(path).with_context(|| format!("cannot open {}", path.display()))?;
    let mut hasher = Sha256::new();
    let mut buf = vec![0u8; 1 << 20];
    loop {
        let n = file.read(&mut buf)?;
        if n == 0 {
            break;
        }
        hasher.update(&buf[..n]);
    }
    Ok(hex(&hasher.finalize()))
}

pub fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

/// A writer that hashes and counts what passes through it.
pub struct HashWriter<W: Write> {
    inner: W,
    hasher: Sha256,
    bytes: u64,
}

impl<W: Write> HashWriter<W> {
    pub fn new(inner: W) -> Self {
        HashWriter {
            inner,
            hasher: Sha256::new(),
            bytes: 0,
        }
    }

    /// The inner writer, the SHA-256 as hex, and the byte count.
    pub fn finish(self) -> (W, String, u64) {
        (self.inner, hex(&self.hasher.finalize()), self.bytes)
    }
}

impl<W: Write> Write for HashWriter<W> {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        let n = self.inner.write(buf)?;
        self.hasher.update(&buf[..n]);
        self.bytes += n as u64;
        Ok(n)
    }

    fn flush(&mut self) -> io::Result<()> {
        self.inner.flush()
    }
}

/// `SHA256SUMS` lines in `sha256sum -c` format.
pub fn checksums_text(sums: &[(String, String)]) -> String {
    sums.iter()
        .map(|(name, sum)| format!("{sum}  {name}\n"))
        .collect()
}

/// Parses `SHA256SUMS` into (file name, sum).
pub fn read_checksums(path: &Path) -> Result<Vec<(String, String)>> {
    let text = fs::read_to_string(path)?;
    Ok(text
        .lines()
        .filter_map(|l| l.split_once("  "))
        .map(|(sum, name)| (name.to_string(), sum.to_string()))
        .collect())
}

pub fn chunk_name(k: u32) -> String {
    format!("chunk-{k:04}.tar.zst")
}

/// An RFC 3339 time as local time, e.g. `2026-11-17 11:04 EST`.
fn friendly_time(rfc3339: &str) -> String {
    match rfc3339.parse::<jiff::Timestamp>() {
        Ok(ts) => crate::cmd::local_time(ts),
        Err(_) => rfc3339.to_string(),
    }
}

/// Reads the header of a finished manifest.
pub fn read_header(path: &Path) -> Result<Header> {
    for line in open_lines(path)?.lines() {
        if let Record::Header(h) = parse_record(&line?)? {
            return Ok(*h);
        }
    }
    bail!("{} has no header", path.display())
}

/// The README kept with every archive, explaining how to restore it with
/// standard tools. `location` is where the chunks are, if not alongside.
pub fn readme_text(header: &Header, location: Option<&str>) -> String {
    let local_dir = shell_quote(&format!(
        "{}/{}/{}",
        header.source,
        super::ARCHIVE_DIR,
        header.archive_id
    ));
    let fetch = match location {
        None => format!("    cd {local_dir}\n"),
        Some(at) => format!(
            "    # Download every file from {at} into one directory, for example\n    # with sftp or an S3 client, and change into it. Then:\n"
        ),
    };
    format!(
        "This is a stomp archive of {source}.

Archive ID: {id}
Created:    {created} by {user} on {host}
Contents:   {files} files ({bytes}), in {chunks} chunk(s){stored}

Each chunk-NNNN.tar.zst is an ordinary tar file compressed with zstd. Paths
inside are relative to the archived directory. manifest.jsonl.zst lists every
file with its checksum, and anything left in place.

To restore with stomp:

    stomp restore {id}

To restore without stomp, using sha256sum, zstd and tar:

{fetch}    sha256sum -c SHA256SUMS
    for c in chunk-*.tar.zst; do zstd -dc \"$c\" | tar -x -C {source_q}; done

To list what's inside:

    zstdcat manifest.jsonl.zst | less
",
        source = header.source,
        id = header.archive_id,
        created = friendly_time(&header.created_at),
        user = header.user.as_deref().unwrap_or("unknown"),
        host = header.host,
        files = format_count(header.totals.files),
        bytes = format_bytes(header.totals.bytes),
        chunks = header.totals.chunks,
        stored = location.map_or(String::new(), |at| format!("\nStored at:  {at}")),
        source_q = shell_quote(&header.source),
    )
}

/// The note left in an archived directory (§12.1). `remote` is where the
/// chunks are, for an archive stored at a destination.
pub fn note_text(
    id: &str,
    source: &Path,
    created: &str,
    user: Option<&str>,
    finished: Option<&Header>,
    remote: Option<&str>,
) -> String {
    let records = format!("{}/{id}", super::ARCHIVE_DIR);
    let location = match remote {
        Some(at) => format!("{at}\n           (records in {records}, inside this directory)"),
        None => format!("{records} (inside this directory)"),
    };
    let created = friendly_time(created);
    let mut text = match finished {
        None => format!(
            "This directory is being archived by stomp (archive {id}), started {created} by {user}.

Files are packed into compressed chunks and the originals are removed one
chunk at a time, after each chunk has been verified where it's stored. If
archiving stops partway, everything already removed is safe in those chunks.

Chunks:                 {stored}
To continue:            stomp archive --resume {dir}
To put everything back: stomp restore {dir}
",
            user = user.unwrap_or("unknown"),
            stored = match remote {
                Some(at) => format!("{at} (records in {records})"),
                None => format!("{records} (inside this directory)"),
            },
            dir = shell_path(source),
        ),
        Some(h) => format!(
            "This directory was archived by stomp on {created} by {user}.

Archive:   {id}
Location:  {location}
Contents:  {files} files ({bytes}), compressed to {compressed} in {chunks} chunk(s)

The originals were packed into compressed tar files and removed.
{kept}{skipped}
To restore with stomp:

    stomp restore {id}

To restore without stomp, see {records}/README.txt.
",
            user = user.unwrap_or("unknown"),
            files = format_count(h.totals.files),
            bytes = format_bytes(h.totals.bytes),
            compressed = format_bytes(h.totals.compressed_bytes),
            chunks = h.totals.chunks,
            kept = if h.totals.kept > 0 {
                format!(
                    "{} file(s) changed while being archived and were left in place.\n",
                    h.totals.kept
                )
            } else {
                String::new()
            },
            skipped = if h.totals.skipped > 0 {
                format!(
                    "{} item(s) were left in place on purpose (environments, caches, files owned\nby others...); the manifest lists them with reasons.\n",
                    h.totals.skipped
                )
            } else {
                String::new()
            },
        ),
    };
    if !text.ends_with('\n') {
        text.push('\n');
    }
    text
}

/// A path as people see it (`~/...`), quoted for a shell where needed with
/// the `~` left outside the quotes so it still expands.
fn shell_path(path: &Path) -> String {
    let shown = crate::paths::display(path);
    match shown.strip_prefix("~/") {
        Some(rest) => format!("~/{}", shell_quote(rest)),
        None => shell_quote(&shown),
    }
}

fn shell_quote(s: &str) -> String {
    if s.chars()
        .all(|c| c.is_ascii_alphanumeric() || "/._-~".contains(c))
    {
        s.to_string()
    } else {
        format!("'{}'", s.replace('\'', r"'\''"))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::ffi::OsStr;

    #[test]
    fn paths_round_trip_including_non_utf8() {
        for raw in [b"plain/name.txt".to_vec(), b"bad\xffname".to_vec()] {
            let p = PathBuf::from(OsStr::from_bytes(&raw));
            let field = PathField::new(&p);
            assert_eq!(field.path.is_some(), std::str::from_utf8(&raw).is_ok());
            let json = serde_json::to_string(&field).unwrap();
            let back: PathField = serde_json::from_str(&json).unwrap();
            assert_eq!(back.get().unwrap(), p);
        }
    }

    #[test]
    fn the_unfinished_note_is_tidy() {
        let home = crate::paths::home_dir().unwrap();
        let source = home.join("my runs");
        let local = note_text(
            "G-20261118-abcdef",
            &source,
            "2026-11-18T15:00:00Z",
            Some("alex"),
            None,
            None,
        );
        assert!(
            local.contains(
                "Chunks:                 .stomp-archive/G-20261118-abcdef (inside this directory)"
            ),
            "{local}"
        );
        // `~` stays outside the quotes, so a shell still expands it.
        assert!(
            local.contains("stomp archive --resume ~/'my runs'"),
            "{local}"
        );
        assert!(
            local.contains("To put everything back: stomp restore ~/'my runs'"),
            "{local}"
        );
        assert!(
            local
                .lines()
                .all(|l| l.chars().count() <= 80 || l.starts_with("This directory")),
            "{local}"
        );

        let remote = note_text(
            "G-20261118-abcdef",
            &source,
            "2026-11-18T15:00:00Z",
            Some("alex"),
            None,
            Some("nas:/archive/alex/G/G-20261118-abcdef"),
        );
        assert!(remote.contains("Chunks:                 nas:/archive/alex/G/G-20261118-abcdef (records in .stomp-archive/G-20261118-abcdef)"), "{remote}");
    }

    #[test]
    fn records_round_trip() {
        let rec = EntryRecord {
            record: entry_tag(),
            path: PathField::new(Path::new("a/b")),
            kind: Kind::Symlink,
            chunk: 2,
            size: 0,
            mode: 0o777,
            mtime: 5,
            uid: 1,
            gid: 2,
            blake3: None,
            target: TargetField::new(Some(Path::new("../x"))),
            changed: false,
            kept: true,
            ino: 9,
            mtime_ns: 5,
            ctime_ns: 6,
        };
        let line = serde_json::to_string(&rec).unwrap();
        assert!(line.contains("\"type\":\"entry\""));
        assert!(!line.contains("changed"));
        let Record::Entry(back) = parse_record(&line).unwrap() else {
            panic!("wrong record type")
        };
        assert_eq!(back.target.get().unwrap(), Some(PathBuf::from("../x")));
        assert!(back.kept);
        let skip = serde_json::to_string(&SkipRecord::new(
            Path::new("env"),
            SkipReason::Environment,
            7,
        ))
        .unwrap();
        assert!(matches!(parse_record(&skip).unwrap(), Record::Skipped(s) if s.bytes == 7));
    }

    #[test]
    fn jsonl_files_round_trip() {
        let tmp = tempfile::tempdir().unwrap();
        for name in ["x.jsonl", "x.jsonl.zst"] {
            let path = tmp.path().join(name);
            write_jsonl(&path, [1u32, 2, 3]).unwrap();
            assert_eq!(read_jsonl::<u32>(&path).unwrap(), [1, 2, 3]);
        }
    }
}
