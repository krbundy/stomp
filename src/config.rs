//! Configuration loading and validation (§7 of the spec).
//!
//! The file is parsed into `Raw*` structs that mirror the TOML, then
//! validated into a [`Config`] with expanded, canonical paths. Validation
//! collects every problem, so the user sees them all at once.

use std::collections::HashSet;
use std::fmt;
use std::path::{Path, PathBuf};
use std::time::Duration;

use serde::Deserialize;

use crate::paths;
use crate::units;

/// Grant IDs reserved for stomp's own report rows (compared case-insensitively).
pub const RESERVED_GRANT_IDS: [&str; 2] = ["stomp", "unassigned"];

/// Destination name reserved for archives kept in place.
pub const LOCAL_DESTINATION: &str = "local";

const DEFAULT_MAX_THREADS: usize = 8;

// ---------------------------------------------------------------------------
// The file as written
// ---------------------------------------------------------------------------

#[derive(Debug, Default, Deserialize)]
#[serde(deny_unknown_fields)]
struct RawConfig {
    #[serde(default)]
    general: RawGeneral,
    #[serde(default)]
    slurm: SlurmConfig,
    #[serde(default)]
    scan: RawScan,
    #[serde(default)]
    archive: RawArchive,
    #[serde(default)]
    compute: RawCompute,
    #[serde(default, rename = "destination")]
    destinations: Vec<Destination>,
    #[serde(default, rename = "grant")]
    grants: Vec<RawGrant>,
    #[serde(default, rename = "policy")]
    policies: Vec<RawPolicy>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct RawPolicy {
    name: Option<String>,
    #[serde(rename = "match", default)]
    conditions: RawConditions,
    action: Option<String>,
    destination: Option<String>,
    wait: Option<String>,
}

#[derive(Debug, Default, Deserialize)]
#[serde(deny_unknown_fields)]
struct RawConditions {
    grant: Option<OneOrMany>,
    path: Option<String>,
    grant_ended_for: Option<String>,
    unchanged_for: Option<String>,
    min_bytes: Option<String>,
    min_files: Option<u64>,
    max_avg_file_size: Option<String>,
}

#[derive(Debug, Deserialize)]
#[serde(untagged)]
enum OneOrMany {
    One(String),
    Many(Vec<String>),
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct RawGeneral {
    #[serde(default = "default_roots")]
    roots: Vec<String>,
    staging_dir: Option<String>,
    rate_per_tb_month: Option<f64>,
    rate_per_cpu_hour: Option<f64>,
    rate_per_gpu_hour: Option<f64>,
}

impl Default for RawGeneral {
    fn default() -> Self {
        Self {
            roots: default_roots(),
            staging_dir: None,
            rate_per_tb_month: None,
            rate_per_cpu_hour: None,
            rate_per_gpu_hour: None,
        }
    }
}

fn default_roots() -> Vec<String> {
    vec!["~".to_string()]
}

#[derive(Debug, Default, Deserialize)]
#[serde(deny_unknown_fields)]
struct RawScan {
    max_threads: Option<usize>,
    full_every: Option<String>,
    rctime_margin: Option<String>,
    #[serde(default)]
    exclude: Vec<String>,
    fold_bytes: Option<String>,
    fold_files: Option<u64>,
}

#[derive(Debug, Default, Deserialize)]
#[serde(deny_unknown_fields)]
struct RawArchive {
    chunk_size: Option<String>,
    zstd_level: Option<i32>,
    only_owned: Option<bool>,
    default_destination: Option<String>,
}

#[derive(Debug, Default, Deserialize)]
#[serde(deny_unknown_fields)]
struct RawCompute {
    infer_from_workdir: Option<bool>,
    history_start: Option<toml::value::Datetime>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct RawGrant {
    id: String,
    title: Option<String>,
    pi: Option<String>,
    start: Option<toml::value::Datetime>,
    end: Option<toml::value::Datetime>,
    #[serde(default)]
    dirs: Vec<String>,
}

/// Settings for the Slurm tasks stomp submits (§14).
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SlurmConfig {
    pub account: Option<String>,
    pub partition: Option<String>,
    pub time: Option<String>,
    pub cpus: Option<u32>,
    pub mem: Option<String>,
    pub auto_resubmit: Option<bool>,
}

/// A remote place archives can be sent (§13).
#[derive(Debug, Clone, Deserialize)]
#[serde(tag = "type", rename_all = "lowercase")]
pub enum Destination {
    Sftp(SftpDestination),
    S3(S3Destination),
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SftpDestination {
    pub name: String,
    pub host: String,
    pub path: String,
    pub parallel_uploads: Option<u32>,
    pub verify: Option<SftpVerify>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum SftpVerify {
    RemoteSha256,
    Readback,
    Size,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct S3Destination {
    pub name: String,
    pub bucket: String,
    pub prefix: Option<String>,
    pub endpoint: Option<String>,
    pub region: Option<String>,
    pub profile: Option<String>,
    pub force_path_style: Option<bool>,
    pub storage_class: Option<String>,
    pub parallel_uploads: Option<u32>,
    pub verify: Option<S3Verify>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum S3Verify {
    ServerChecksum,
    Readback,
    Size,
}

impl Destination {
    pub fn name(&self) -> &str {
        match self {
            Self::Sftp(d) => &d.name,
            Self::S3(d) => &d.name,
        }
    }

    pub fn kind(&self) -> &'static str {
        match self {
            Self::Sftp(_) => "sftp",
            Self::S3(_) => "s3",
        }
    }
}

// ---------------------------------------------------------------------------
// The validated configuration
// ---------------------------------------------------------------------------

#[derive(Debug, Clone)]
pub struct Config {
    /// The file this was loaded from.
    pub source: PathBuf,
    /// Canonical root directories.
    pub roots: Vec<PathBuf>,
    pub grants: Vec<Grant>,
    pub destinations: Vec<Destination>,
    pub slurm: SlurmConfig,
    pub scan: ScanConfig,
    pub compute: ComputeConfig,
    pub archive: ArchiveConfig,
    pub rates: Rates,
    /// Where archive chunks are built; `None` means inside the archived directory.
    pub staging_dir: Option<PathBuf>,
    /// Rules proposing directories to archive (§16).
    pub policies: Vec<Policy>,
    /// Problems that don't stop stomp from working, such as a missing grant directory.
    pub warnings: Vec<String>,
}

#[derive(Debug, Clone)]
pub struct ScanConfig {
    pub max_threads: usize,
    /// User exclusion globs, with `~` and variables expanded.
    pub exclude: Vec<String>,
    pub full_every: Duration,
    pub rctime_margin: Duration,
    /// Directories smaller than both of these are folded into their parent (§9.3).
    pub fold_bytes: u64,
    pub fold_files: u64,
}

#[derive(Debug, Clone)]
pub struct ComputeConfig {
    pub infer_from_workdir: bool,
    pub history_start: Option<jiff::civil::Date>,
}

/// Archive settings (§12).
#[derive(Debug, Clone)]
pub struct ArchiveConfig {
    pub chunk_size: u64,
    pub zstd_level: i32,
    /// Leave files owned by other users in place (D10).
    pub only_owned: bool,
    /// `local` or a destination name.
    pub default_destination: String,
}

/// A rule that proposes directories to archive (§16).
#[derive(Debug, Clone)]
pub struct Policy {
    pub name: String,
    pub conditions: Conditions,
    pub action: PolicyAction,
    /// `local` or a destination name.
    pub destination: String,
    /// How long an `apply` proposal waits before stomp archives it.
    pub wait: Duration,
}

/// What a directory must satisfy to be proposed. Every condition given must hold.
#[derive(Debug, Clone, Default)]
pub struct Conditions {
    /// Grant IDs; empty means any grant.
    pub grants: Vec<String>,
    /// A glob with `~` expanded; `*` doesn't cross `/`.
    pub path: Option<String>,
    pub grant_ended_for: Option<Duration>,
    pub unchanged_for: Option<Duration>,
    pub min_bytes: Option<u64>,
    pub min_files: Option<u64>,
    pub max_avg_file_size: Option<u64>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PolicyAction {
    /// List the directory in `stomp candidates`.
    Propose,
    /// Also archive it automatically once `wait` has passed.
    Apply,
}

impl PolicyAction {
    pub fn as_str(self) -> &'static str {
        match self {
            PolicyAction::Propose => "propose",
            PolicyAction::Apply => "apply",
        }
    }
}

/// How long an automatic proposal waits by default, so it can be dismissed.
pub const DEFAULT_POLICY_WAIT: Duration = Duration::from_secs(7 * 86_400);

/// Optional rates for cost estimates in reports.
#[derive(Debug, Clone, Default)]
pub struct Rates {
    pub per_tb_month: Option<f64>,
    pub per_cpu_hour: Option<f64>,
    pub per_gpu_hour: Option<f64>,
}

#[derive(Debug, Clone)]
pub struct Grant {
    pub id: String,
    pub title: Option<String>,
    pub pi: Option<String>,
    pub start: Option<jiff::civil::Date>,
    pub end: Option<jiff::civil::Date>,
    /// The grant's directories, without duplicates or redundant nested entries.
    pub dirs: Vec<GrantDir>,
}

#[derive(Debug, Clone)]
pub struct GrantDir {
    /// Canonical path if the directory exists, normalized path otherwise.
    pub path: PathBuf,
    pub exists: bool,
    /// Index into [`Config::roots`] of the root containing this directory.
    pub root: usize,
}

/// Errors that make a config unusable.
#[derive(Debug)]
pub struct ConfigError {
    pub path: PathBuf,
    pub problems: Vec<String>,
    /// Warnings found alongside the problems, so fixing one round of
    /// problems doesn't reveal another.
    pub warnings: Vec<String>,
}

impl ConfigError {
    fn one(path: &Path, problem: impl Into<String>) -> Self {
        Self {
            path: path.to_path_buf(),
            problems: vec![problem.into()],
            warnings: Vec::new(),
        }
    }
}

impl fmt::Display for ConfigError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let file = paths::display(&self.path);
        match self.problems.as_slice() {
            [one] => write!(f, "{file}: {one}")?,
            many => {
                write!(f, "{file} has {} problems:", many.len())?;
                for p in many {
                    write!(f, "\n  - {p}")?;
                }
            }
        }
        for w in &self.warnings {
            write!(f, "\n  warning: {w}")?;
        }
        Ok(())
    }
}

impl std::error::Error for ConfigError {}

impl Config {
    /// Loads and validates the config file at `path`.
    pub fn load(path: &Path) -> Result<Config, ConfigError> {
        let text = match std::fs::read_to_string(path) {
            Ok(text) => text,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                return Err(ConfigError::one(
                    path,
                    "no config file here; run `stomp init` to create one",
                ));
            }
            Err(e) => return Err(ConfigError::one(path, format!("cannot read: {e}"))),
        };
        Self::parse(&text, path)
    }

    /// Parses and validates config text. `source` is used in messages.
    pub fn parse(text: &str, source: &Path) -> Result<Config, ConfigError> {
        let raw: RawConfig =
            toml::from_str(text).map_err(|e| ConfigError::one(source, e.to_string().trim_end()))?;
        let mut v = Validator::default();
        let mut config = v.build(raw, source);
        if v.errors.is_empty() {
            config.warnings = v.warnings;
            Ok(config)
        } else {
            Err(ConfigError {
                path: source.to_path_buf(),
                problems: v.errors,
                warnings: v.warnings,
            })
        }
    }

    /// The grant with this ID.
    pub fn grant(&self, id: &str) -> Option<&Grant> {
        self.grants.iter().find(|g| g.id == id)
    }

    /// The grant whose directory contains `path` (or is `path`), if any.
    pub fn grant_for(&self, path: &Path) -> Option<&Grant> {
        self.grants
            .iter()
            .flat_map(|g| g.dirs.iter().map(move |d| (g, d)))
            .filter(|(_, d)| path.starts_with(&d.path))
            .max_by_key(|(_, d)| d.path.as_os_str().len())
            .map(|(g, _)| g)
    }

    /// Grants that have a directory strictly inside `path`.
    pub fn grants_within(&self, path: &Path) -> Vec<&Grant> {
        self.grants
            .iter()
            .filter(|g| {
                g.dirs
                    .iter()
                    .any(|d| d.path != path && d.path.starts_with(path))
            })
            .collect()
    }
}

/// Whether `id` is acceptable as a grant ID: it's used in file names and archive IDs.
pub fn valid_grant_id(id: &str) -> bool {
    !id.is_empty()
        && id.len() <= 64
        && !id.starts_with(['.', '-'])
        && id
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.'))
}

fn to_date(value: &toml::value::Datetime) -> Result<jiff::civil::Date, String> {
    match (value.date, value.time, value.offset) {
        (Some(d), None, None) => jiff::civil::Date::new(d.year as i16, d.month as i8, d.day as i8)
            .map_err(|e| e.to_string()),
        _ => Err(format!("{value} should be a plain date such as 2027-06-30")),
    }
}

#[derive(Default)]
struct Validator {
    errors: Vec<String>,
    warnings: Vec<String>,
}

impl Validator {
    fn error(&mut self, message: impl Into<String>) {
        self.errors.push(message.into());
    }

    fn warn(&mut self, message: impl Into<String>) {
        self.warnings.push(message.into());
    }

    /// Expands a configured path and checks that it's absolute.
    fn expand(&mut self, field: &str, raw: &str) -> Option<PathBuf> {
        match paths::expand(raw) {
            Ok(p) if p.is_absolute() => Some(p),
            Ok(_) => {
                self.error(format!(
                    "{field}: {raw:?} must be an absolute path or start with ~"
                ));
                None
            }
            Err(e) => {
                self.error(format!("{field}: {e:#}"));
                None
            }
        }
    }

    fn build(&mut self, raw: RawConfig, source: &Path) -> Config {
        let roots = self.roots(&raw.general.roots);
        let grants = self.grants(&raw.grants, &roots);
        self.destinations(&raw.destinations);
        self.references(&raw);
        self.settings(&raw);
        let policies = self.policies(&raw, &grants);
        // Invalid values were reported by `settings`; fall back to defaults here.
        let duration = |value: &Option<String>, default: u64| {
            value
                .as_deref()
                .and_then(|v| units::parse_duration(v).ok())
                .unwrap_or(Duration::from_secs(default))
        };
        let exclude = raw
            .scan
            .exclude
            .iter()
            .filter_map(|pattern| self.exclude_pattern(pattern))
            .collect();
        Config {
            source: source.to_path_buf(),
            roots,
            grants,
            destinations: raw.destinations,
            slurm: raw.slurm,
            scan: ScanConfig {
                max_threads: raw.scan.max_threads.unwrap_or(DEFAULT_MAX_THREADS),
                exclude,
                full_every: duration(&raw.scan.full_every, 30 * 86_400),
                rctime_margin: duration(&raw.scan.rctime_margin, 3600),
                fold_bytes: raw
                    .scan
                    .fold_bytes
                    .as_deref()
                    .and_then(|v| units::parse_size(v).ok())
                    .unwrap_or(1024 * 1024),
                fold_files: raw.scan.fold_files.unwrap_or(100),
            },
            compute: ComputeConfig {
                infer_from_workdir: raw.compute.infer_from_workdir.unwrap_or(true),
                history_start: raw
                    .compute
                    .history_start
                    .as_ref()
                    .and_then(|d| to_date(d).ok()),
            },
            archive: ArchiveConfig {
                chunk_size: raw
                    .archive
                    .chunk_size
                    .as_deref()
                    .and_then(|v| units::parse_size(v).ok())
                    .unwrap_or(50 << 30),
                zstd_level: raw.archive.zstd_level.unwrap_or(3),
                only_owned: raw.archive.only_owned.unwrap_or(true),
                default_destination: raw
                    .archive
                    .default_destination
                    .clone()
                    .unwrap_or_else(|| LOCAL_DESTINATION.to_string()),
            },
            rates: Rates {
                per_tb_month: raw.general.rate_per_tb_month,
                per_cpu_hour: raw.general.rate_per_cpu_hour,
                per_gpu_hour: raw.general.rate_per_gpu_hour,
            },
            staging_dir: raw
                .general
                .staging_dir
                .as_deref()
                .and_then(|d| paths::expand(d).ok()),
            policies,
            warnings: Vec::new(),
        }
    }

    /// Expands a leading `~` or variables in an exclusion glob and checks its syntax.
    fn exclude_pattern(&mut self, pattern: &str) -> Option<String> {
        let expanded = if pattern.starts_with('~') || pattern.contains('$') {
            match paths::expand(pattern) {
                Ok(p) => p.to_string_lossy().into_owned(),
                Err(e) => {
                    self.error(format!("scan.exclude: {e:#}"));
                    return None;
                }
            }
        } else {
            pattern.to_string()
        };
        match globset::Glob::new(&expanded) {
            Ok(_) => Some(expanded),
            Err(e) => {
                self.error(format!("scan.exclude: {pattern:?}: {e}"));
                None
            }
        }
    }

    fn roots(&mut self, raw: &[String]) -> Vec<PathBuf> {
        if raw.is_empty() {
            self.error("general.roots must list at least one directory");
        }
        let mut roots: Vec<PathBuf> = Vec::new();
        for r in raw {
            let Some(path) = self.expand("general.roots", r) else {
                continue;
            };
            match std::fs::canonicalize(&path) {
                Ok(c) if c.is_dir() => {
                    if let Some(other) = roots
                        .iter()
                        .find(|o| c.starts_with(o.as_path()) || o.starts_with(&c))
                    {
                        if *other == c {
                            self.error(format!("general.roots lists {r:?} twice"));
                        } else {
                            self.error(format!(
                                "general.roots: {} and {} are nested; roots must not contain each other",
                                paths::display(other),
                                paths::display(&c)
                            ));
                        }
                    } else {
                        roots.push(c);
                    }
                }
                Ok(_) => self.error(format!("general.roots: {r:?} is not a directory")),
                Err(e) => self.error(format!("general.roots: {r:?}: {e}")),
            }
        }
        roots
    }

    fn grants(&mut self, raw: &[RawGrant], roots: &[PathBuf]) -> Vec<Grant> {
        let mut ids = HashSet::new();
        let mut grants = Vec::new();
        for g in raw {
            let id = g.id.as_str();
            if !valid_grant_id(id) {
                self.error(format!(
                    "grant id {id:?} may only contain letters, digits, '-', '_' and '.' (64 characters at most)"
                ));
            } else if RESERVED_GRANT_IDS
                .iter()
                .any(|r| r.eq_ignore_ascii_case(id))
            {
                self.error(format!(
                    "grant id {id:?} is reserved by stomp; choose another"
                ));
            } else if !ids.insert(id.to_string()) {
                self.error(format!("grant id {id:?} is used by more than one grant"));
            }

            let mut date = |field: &str, value: &Option<toml::value::Datetime>| {
                value.as_ref().and_then(|v| match to_date(v) {
                    Ok(d) => Some(d),
                    Err(e) => {
                        self.error(format!("grant {id}: {field}: {e}"));
                        None
                    }
                })
            };
            let start = date("start", &g.start);
            let end = date("end", &g.end);
            if let (Some(s), Some(e)) = (start, end)
                && s > e
            {
                self.error(format!("grant {id}: start ({s}) is after end ({e})"));
            }

            if g.dirs.is_empty() {
                self.warn(format!(
                    "grant {id} has no dirs, so no storage is attributed to it"
                ));
            }
            let mut dirs = Vec::new();
            for d in &g.dirs {
                let Some(expanded) = self.expand(&format!("grant {id}: dirs"), d) else {
                    continue;
                };
                let (path, exists) = paths::resolve(&expanded);
                if exists && !path.is_dir() {
                    self.error(format!("grant {id}: {d:?} is not a directory"));
                    continue;
                }
                if !exists {
                    self.warn(format!("grant {id}: {d} does not exist"));
                }
                let Some(root) = roots.iter().position(|r| path.starts_with(r)) else {
                    if !roots.is_empty() {
                        let hint = if exists && path != expanded {
                            format!(" (it resolves to {}; is it a symlink?)", path.display())
                        } else {
                            String::new()
                        };
                        self.error(format!(
                            "grant {id}: {d} is not inside any of general.roots{hint}"
                        ));
                    }
                    continue;
                };
                dirs.push(GrantDir { path, exists, root });
            }

            grants.push(Grant {
                id: id.to_string(),
                title: g.title.clone(),
                pi: g.pi.clone(),
                start,
                end,
                dirs,
            });
        }
        self.overlaps(&mut grants);
        grants
    }

    /// Enforces that no directory is claimed by two grants (§7.3), and drops a
    /// grant's own duplicate or nested directories so they aren't counted twice.
    fn overlaps(&mut self, grants: &mut [Grant]) {
        let all: Vec<(usize, PathBuf)> = grants
            .iter()
            .enumerate()
            .flat_map(|(gi, g)| g.dirs.iter().map(move |d| (gi, d.path.clone())))
            .collect();
        for (i, (ga, a)) in all.iter().enumerate() {
            for (gb, b) in &all[i + 1..] {
                let (outer, inner, inner_grant) = if b.starts_with(a) {
                    (a, b, *gb)
                } else if a.starts_with(b) {
                    (b, a, *ga)
                } else {
                    continue;
                };
                let (ida, idb) = (&grants[*ga].id, &grants[*gb].id);
                if ga == gb {
                    let what = if outer == inner {
                        "twice"
                    } else {
                        "nested inside another of its directories"
                    };
                    self.warn(format!(
                        "grant {ida}: {} is listed {what}; it is counted once",
                        paths::display(inner)
                    ));
                } else if outer == inner {
                    self.error(format!(
                        "{} is claimed by both {ida} and {idb}; a directory can belong to only one grant",
                        paths::display(a)
                    ));
                } else {
                    let (outer_id, inner_id) = if inner_grant == *gb {
                        (ida, idb)
                    } else {
                        (idb, ida)
                    };
                    self.error(format!(
                        "{} (grant {inner_id}) is inside {} (grant {outer_id}); grant directories must not overlap",
                        paths::display(inner),
                        paths::display(outer)
                    ));
                }
            }
        }
        // Within a grant, keep only the outermost copy of each directory.
        for grant in grants.iter_mut() {
            let mut kept: Vec<GrantDir> = Vec::new();
            for d in grant.dirs.drain(..) {
                if kept.iter().any(|k| d.path.starts_with(&k.path)) {
                    continue; // a duplicate of, or nested in, a directory already kept
                }
                kept.retain(|k| !k.path.starts_with(&d.path));
                kept.push(d);
            }
            grant.dirs = kept;
        }
    }

    fn destinations(&mut self, destinations: &[Destination]) {
        let mut names = HashSet::new();
        for d in destinations {
            let name = d.name();
            if name.is_empty() || name.contains(char::is_whitespace) {
                self.error(format!(
                    "destination name {name:?} must be non-empty with no spaces"
                ));
            } else if name == LOCAL_DESTINATION {
                self.error(format!(
                    "destination name {LOCAL_DESTINATION:?} is reserved for archives kept in place"
                ));
            } else if !names.insert(name) {
                self.error(format!("destination name {name:?} is used more than once"));
            }
            match d {
                Destination::Sftp(s) => {
                    if s.host.is_empty() {
                        self.error(format!("destination {name}: host must not be empty"));
                    }
                    if s.path.is_empty() {
                        self.error(format!("destination {name}: path must not be empty"));
                    }
                    if s.parallel_uploads.is_some() {
                        self.warn(format!(
                            "destination {name}: parallel_uploads has no effect on SFTP destinations, which upload one file at a time"
                        ));
                    }
                }
                Destination::S3(s) => {
                    if s.bucket.is_empty() {
                        self.error(format!("destination {name}: bucket must not be empty"));
                    }
                    if let Some(class) = &s.storage_class
                        && class != "STANDARD"
                    {
                        self.error(format!(
                            "destination {name}: storage_class {class:?} isn't supported yet; only \"STANDARD\" (cold classes such as GLACIER need thawing before a restore, which stomp doesn't do yet)"
                        ));
                    }
                }
            }
        }
    }

    /// Checks that destinations named elsewhere exist.
    fn references(&mut self, raw: &RawConfig) {
        let exists = |name: &str| {
            name == LOCAL_DESTINATION || raw.destinations.iter().any(|d| d.name() == name)
        };
        if let Some(name) = &raw.archive.default_destination
            && !exists(name)
        {
            self.error(format!(
                "archive.default_destination: no destination named {name:?}"
            ));
        }
    }

    /// Validates the `[[policy]]` blocks (§16).
    fn policies(&mut self, raw: &RawConfig, grants: &[Grant]) -> Vec<Policy> {
        let exists = |name: &str| {
            name == LOCAL_DESTINATION || raw.destinations.iter().any(|d| d.name() == name)
        };
        let default_destination = raw
            .archive
            .default_destination
            .clone()
            .unwrap_or_else(|| LOCAL_DESTINATION.to_string());
        let mut names = HashSet::new();
        let mut out = Vec::new();
        for (i, p) in raw.policies.iter().enumerate() {
            let label = match &p.name {
                Some(name) => format!("policy {name:?}"),
                None => {
                    self.error(format!("policy #{} needs a name", i + 1));
                    format!("policy #{}", i + 1)
                }
            };
            let mut ok = true;
            if let Some(name) = &p.name {
                if !valid_grant_id(name) {
                    ok = false;
                    self.error(format!(
                        "{label}: names may only contain letters, digits, '-', '_' and '.'"
                    ));
                } else if !names.insert(name.clone()) {
                    ok = false;
                    self.error(format!("{label} is defined more than once"));
                }
            }
            let destination = p.destination.clone().unwrap_or(default_destination.clone());
            if !exists(&destination) {
                ok = false;
                self.error(format!("{label}: no destination named {destination:?}"));
            }
            let action = match p.action.as_deref() {
                None | Some("propose") => PolicyAction::Propose,
                Some("apply") => PolicyAction::Apply,
                Some(other) => {
                    ok = false;
                    self.error(format!(
                        "{label}: action must be \"propose\" or \"apply\", not {other:?}"
                    ));
                    PolicyAction::Propose
                }
            };
            let mut duration = |field: &str, value: &Option<String>| -> Option<Duration> {
                let v = value.as_deref()?;
                match units::parse_duration(v) {
                    Ok(d) => Some(d),
                    Err(e) => {
                        ok = false;
                        self.error(format!("{label}: {field}: {e:#}"));
                        None
                    }
                }
            };
            let wait = duration("wait", &p.wait).unwrap_or(DEFAULT_POLICY_WAIT);
            let c = &p.conditions;
            let grant_ended_for = duration("match.grant_ended_for", &c.grant_ended_for);
            let unchanged_for = duration("match.unchanged_for", &c.unchanged_for);
            let mut size = |field: &str, value: &Option<String>| -> Option<u64> {
                let v = value.as_deref()?;
                match units::parse_size(v) {
                    Ok(n) => Some(n),
                    Err(e) => {
                        ok = false;
                        self.error(format!("{label}: {field}: {e:#}"));
                        None
                    }
                }
            };
            let min_bytes = size("match.min_bytes", &c.min_bytes);
            let max_avg_file_size = size("match.max_avg_file_size", &c.max_avg_file_size);
            let policy_grants = match &c.grant {
                None => Vec::new(),
                Some(OneOrMany::One(g)) => vec![g.clone()],
                Some(OneOrMany::Many(gs)) => gs.clone(),
            };
            for g in &policy_grants {
                if !grants.iter().any(|known| &known.id == g) {
                    ok = false;
                    self.error(format!("{label}: match.grant: no grant {g:?}"));
                }
            }
            let path = c.path.as_deref().and_then(|pattern| {
                let expanded = match paths::expand(pattern) {
                    Ok(p) => p.to_string_lossy().into_owned(),
                    Err(e) => {
                        self.error(format!("{label}: match.path: {e:#}"));
                        return None;
                    }
                };
                match globset::GlobBuilder::new(&expanded)
                    .literal_separator(true)
                    .build()
                {
                    Ok(_) if expanded.starts_with('/') => Some(expanded),
                    Ok(_) => {
                        self.error(format!(
                            "{label}: match.path: {pattern:?} must be an absolute path or start with ~"
                        ));
                        None
                    }
                    Err(e) => {
                        self.error(format!("{label}: match.path: {pattern:?}: {e}"));
                        None
                    }
                }
            });
            if c.path.is_some() && path.is_none() {
                ok = false;
            }
            let conditions = Conditions {
                grants: policy_grants,
                path,
                grant_ended_for,
                unchanged_for,
                min_bytes,
                min_files: c.min_files,
                max_avg_file_size,
            };
            let any = !conditions.grants.is_empty()
                || conditions.path.is_some()
                || conditions.grant_ended_for.is_some()
                || conditions.unchanged_for.is_some()
                || conditions.min_bytes.is_some()
                || conditions.min_files.is_some()
                || conditions.max_avg_file_size.is_some();
            if !any {
                ok = false;
                self.error(format!(
                    "{label} has no conditions; add a match = {{ ... }} table"
                ));
            } else if action == PolicyAction::Apply
                && c.grant_ended_for.is_none()
                && c.unchanged_for.is_none()
            {
                ok = false;
                self.error(format!(
                    "{label}: action = \"apply\" needs match.grant_ended_for or match.unchanged_for, so only data nobody is changing is archived automatically"
                ));
            }
            if ok && let Some(name) = &p.name {
                out.push(Policy {
                    name: name.clone(),
                    conditions,
                    action,
                    destination,
                    wait,
                });
            }
        }
        out
    }

    /// Checks values whose format TOML can't express.
    fn settings(&mut self, raw: &RawConfig) {
        if raw.scan.max_threads == Some(0) {
            self.error("scan.max_threads must be at least 1");
        }
        for (field, value) in [
            ("scan.full_every", &raw.scan.full_every),
            ("scan.rctime_margin", &raw.scan.rctime_margin),
        ] {
            if let Some(v) = value
                && let Err(e) = units::parse_duration(v)
            {
                self.error(format!("{field}: {e:#}"));
            }
        }
        for (field, value) in [
            ("archive.chunk_size", &raw.archive.chunk_size),
            ("scan.fold_bytes", &raw.scan.fold_bytes),
        ] {
            if let Some(size) = value
                && let Err(e) = units::parse_size(size)
            {
                self.error(format!("{field}: {e:#}"));
            }
        }
        if let Some(size) = &raw.archive.chunk_size
            && units::parse_size(size).is_ok_and(|s| s < 1 << 20)
        {
            self.error("archive.chunk_size must be at least 1 MiB");
        }
        if let Some(level) = raw.archive.zstd_level
            && !(1..=22).contains(&level)
        {
            self.error(format!(
                "archive.zstd_level must be between 1 and 22, not {level}"
            ));
        }
        if let Some(d) = &raw.compute.history_start
            && let Err(e) = to_date(d)
        {
            self.error(format!("compute.history_start: {e}"));
        }
        for (field, value) in [
            ("general.rate_per_tb_month", raw.general.rate_per_tb_month),
            ("general.rate_per_cpu_hour", raw.general.rate_per_cpu_hour),
            ("general.rate_per_gpu_hour", raw.general.rate_per_gpu_hour),
        ] {
            if value.is_some_and(|v| v.is_nan() || v < 0.0) {
                self.error(format!("{field} must be zero or more"));
            }
        }
        if let Some(dir) = &raw.general.staging_dir {
            self.expand("general.staging_dir", dir);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    /// A temporary directory tree plus a helper to parse config text referring to it.
    struct Fixture {
        dir: tempfile::TempDir,
    }

    impl Fixture {
        fn new(subdirs: &[&str]) -> Self {
            let dir = tempfile::tempdir().unwrap();
            for s in subdirs {
                fs::create_dir_all(dir.path().join(s)).unwrap();
            }
            Self { dir }
        }

        fn root(&self) -> PathBuf {
            fs::canonicalize(self.dir.path()).unwrap()
        }

        /// Replaces `ROOT` in the text with the fixture's path, then parses it.
        fn parse(&self, text: &str) -> Result<Config, ConfigError> {
            let text = text.replace("ROOT", self.root().to_str().unwrap());
            Config::parse(&text, Path::new("test.toml"))
        }
    }

    fn problems(result: Result<Config, ConfigError>) -> String {
        result.unwrap_err().problems.join("\n")
    }

    #[test]
    fn parses_full_example() {
        let f = Fixture::new(&["alpha", "beta", "beta_runs"]);
        let cfg = f
            .parse(
                r#"
                [general]
                roots = ["ROOT"]
                rate_per_tb_month = 1.5

                [slurm]
                account = "acct"
                cpus = 8

                [scan]
                max_threads = 4
                full_every = "30d"
                rctime_margin = "1h"
                exclude = ["**/node_modules"]

                [archive]
                chunk_size = "50GiB"
                zstd_level = 3
                default_destination = "nas"

                [compute]
                infer_from_workdir = true
                history_start = 2025-01-01

                [[destination]]
                name = "nas"
                type = "sftp"
                host = "nas"
                path = "/archive/x"
                verify = "remote-sha256"

                [[destination]]
                name = "spaces"
                type = "s3"
                endpoint = "https://nyc3.digitaloceanspaces.com"
                region = "us-east-1"
                bucket = "b"
                verify = "server-checksum"

                [[grant]]
                id = "GRANT-A"
                title = "Alpha"
                start = 2024-07-01
                end = 2027-06-30
                dirs = ["ROOT/alpha"]

                [[grant]]
                id = "GRANT-B"
                dirs = ["ROOT/beta", "ROOT/beta_runs"]

                [[policy]]
                name = "old"
                match = { grant_ended_for = "90d" }
                destination = "spaces"
                "#,
            )
            .unwrap();
        assert_eq!(cfg.roots, vec![f.root()]);
        assert_eq!(cfg.grants.len(), 2);
        assert_eq!(cfg.grants[1].dirs.len(), 2);
        assert_eq!(cfg.scan.max_threads, 4);
        assert_eq!(cfg.destinations.len(), 2);
        assert_eq!(cfg.destinations[1].kind(), "s3");
        assert_eq!(cfg.grants[0].end.unwrap().to_string(), "2027-06-30");
        assert!(cfg.warnings.is_empty(), "{:?}", cfg.warnings);
    }

    #[test]
    fn rejects_unknown_fields() {
        let f = Fixture::new(&[]);
        let msg = problems(f.parse("[general]\nroots = [\"ROOT\"]\nrooots = []\n"));
        assert!(msg.contains("rooots"), "{msg}");
        let msg = problems(f.parse(
            "[[destination]]\nname = \"n\"\ntype = \"sftp\"\nhost = \"h\"\npath = \"/p\"\nhots = \"x\"\n",
        ));
        assert!(msg.contains("hots"), "{msg}");
    }

    #[test]
    fn rejects_directory_claimed_by_two_grants() {
        let f = Fixture::new(&["shared"]);
        let msg = problems(f.parse(
            r#"
            general.roots = ["ROOT"]
            [[grant]]
            id = "A"
            dirs = ["ROOT/shared"]
            [[grant]]
            id = "B"
            dirs = ["ROOT/shared"]
            "#,
        ));
        assert!(msg.contains("claimed by both A and B"), "{msg}");
    }

    #[test]
    fn rejects_nesting_across_grants() {
        let f = Fixture::new(&["outer/inner"]);
        let msg = problems(f.parse(
            r#"
            general.roots = ["ROOT"]
            [[grant]]
            id = "A"
            dirs = ["ROOT/outer/inner"]
            [[grant]]
            id = "B"
            dirs = ["ROOT/outer"]
            "#,
        ));
        assert!(msg.contains("(grant A) is inside"), "{msg}");
        assert!(msg.contains("(grant B)"), "{msg}");
    }

    #[test]
    fn collapses_nesting_within_a_grant() {
        let f = Fixture::new(&["outer/inner"]);
        for dirs in [
            r#"["ROOT/outer", "ROOT/outer/inner"]"#,
            r#"["ROOT/outer/inner", "ROOT/outer"]"#,
            r#"["ROOT/outer", "ROOT/outer"]"#,
        ] {
            let cfg = f
                .parse(&format!(
                    "general.roots = [\"ROOT\"]\n[[grant]]\nid = \"A\"\ndirs = {dirs}\n"
                ))
                .unwrap();
            assert_eq!(cfg.grants[0].dirs.len(), 1, "{dirs}");
            assert_eq!(cfg.grants[0].dirs[0].path, f.root().join("outer"), "{dirs}");
            assert_eq!(cfg.warnings.len(), 1, "{dirs}: {:?}", cfg.warnings);
        }
    }

    #[test]
    fn missing_directory_is_a_warning() {
        let f = Fixture::new(&[]);
        let cfg = f
            .parse("general.roots = [\"ROOT\"]\n[[grant]]\nid = \"A\"\ndirs = [\"ROOT/gone\"]\n")
            .unwrap();
        assert!(!cfg.grants[0].dirs[0].exists);
        assert!(cfg.warnings[0].contains("does not exist"));
    }

    #[test]
    fn rejects_directory_outside_roots() {
        let f = Fixture::new(&["r"]);
        let msg = problems(
            f.parse("general.roots = [\"ROOT/r\"]\n[[grant]]\nid = \"A\"\ndirs = [\"/tmp\"]\n"),
        );
        assert!(msg.contains("not inside any of general.roots"), "{msg}");
    }

    #[test]
    fn rejects_nested_roots_and_bad_ids() {
        let f = Fixture::new(&["sub"]);
        let msg = problems(f.parse(
            r#"
            general.roots = ["ROOT", "ROOT/sub"]
            [[grant]]
            id = "has space"
            [[grant]]
            id = "Stomp"
            [[grant]]
            id = "dup"
            [[grant]]
            id = "dup"
            "#,
        ));
        assert!(msg.contains("are nested"), "{msg}");
        assert!(msg.contains("\"has space\" may only contain"), "{msg}");
        assert!(msg.contains("\"Stomp\" is reserved"), "{msg}");
        assert!(
            msg.contains("\"dup\" is used by more than one grant"),
            "{msg}"
        );
    }

    #[test]
    fn checks_destination_references_and_values() {
        let f = Fixture::new(&[]);
        let msg = problems(f.parse(
            r#"
            general.roots = ["ROOT"]
            scan.full_every = "monthly"
            archive.default_destination = "nowhere"
            archive.zstd_level = 40
            [[destination]]
            name = "local"
            type = "s3"
            bucket = "b"
            [[policy]]
            name = "p"
            destination = "missing"
            action = "delete-everything"
            "#,
        ));
        for expected in [
            "scan.full_every",
            "no destination named \"nowhere\"",
            "zstd_level",
            "\"local\" is reserved",
            "policy \"p\": no destination named \"missing\"",
            "action must be",
        ] {
            assert!(msg.contains(expected), "missing {expected:?} in:\n{msg}");
        }
    }

    #[test]
    fn rejects_dates_with_times_and_reversed_ranges() {
        let f = Fixture::new(&[]);
        let msg = problems(f.parse(
            r#"
            general.roots = ["ROOT"]
            [[grant]]
            id = "A"
            start = 2027-01-01
            end = 2026-01-01
            [[grant]]
            id = "B"
            end = 2026-01-01T10:00:00
            "#,
        ));
        assert!(msg.contains("is after end"), "{msg}");
        assert!(msg.contains("plain date"), "{msg}");
    }

    #[test]
    fn finds_grant_for_path() {
        let f = Fixture::new(&["alpha/sub", "beta"]);
        let cfg = f
            .parse(
                r#"
                general.roots = ["ROOT"]
                [[grant]]
                id = "A"
                dirs = ["ROOT/alpha"]
                [[grant]]
                id = "B"
                dirs = ["ROOT/beta"]
                "#,
            )
            .unwrap();
        let root = f.root();
        assert_eq!(cfg.grant_for(&root.join("alpha/sub")).unwrap().id, "A");
        assert_eq!(cfg.grant_for(&root.join("beta")).unwrap().id, "B");
        assert!(cfg.grant_for(&root).is_none());
        let within: Vec<_> = cfg
            .grants_within(&root)
            .iter()
            .map(|g| g.id.clone())
            .collect();
        assert_eq!(within, ["A", "B"]);
    }

    #[test]
    fn grant_id_rules() {
        assert!(valid_grant_id("NSF-2201234"));
        assert!(valid_grant_id("usda_2024.1"));
        assert!(!valid_grant_id(""));
        assert!(!valid_grant_id(".hidden"));
        assert!(!valid_grant_id("a/b"));
        assert!(!valid_grant_id(&"x".repeat(65)));
    }
}
