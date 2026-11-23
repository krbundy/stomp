//! `stomp init`: writes a starter config from the current directory layout (§7.4).

use std::io::Write as _;
use std::os::unix::fs::{MetadataExt, OpenOptionsExt};
use std::path::Path;

use anyhow::{Context, Result, bail};
use rayon::prelude::*;

use super::age;
use crate::config::{RESERVED_GRANT_IDS, valid_grant_id};
use crate::fs::{self, Backend, BackendKind, Usage, read_entries};
use crate::paths;
use crate::units::{format_bytes, format_count};

struct DirInfo {
    name: String,
    /// The path as it will appear in the config (`~/…` under the home directory).
    display: String,
    /// Known only when the filesystem can measure instantly.
    usage: Option<Usage>,
}

struct RootListing {
    display: String,
    backend: BackendKind,
    visible: Vec<DirInfo>,
    hidden: Vec<DirInfo>,
    loose: Usage,
    unnamed: usize,
}

pub fn run(config_path: &Path, roots: &[String], print_only: bool) -> Result<()> {
    if !print_only && config_path.exists() {
        bail!(
            "{} already exists, and stomp init never overwrites a config. Use `stomp init --print` to see a fresh template.",
            paths::display(config_path)
        );
    }
    let raw_roots = if roots.is_empty() {
        vec!["~".to_string()]
    } else {
        roots.to_vec()
    };
    let mut listings = Vec::new();
    for raw in &raw_roots {
        let root = std::fs::canonicalize(paths::expand(raw)?)
            .with_context(|| format!("cannot open {raw}"))?;
        if !root.is_dir() {
            bail!("{raw} is not a directory");
        }
        let backend = fs::detect(&root)?;
        listings.push(list_root(&root, backend.as_ref())?);
    }

    let today = jiff::ZonedDateTime::now().date().to_string();
    let text = render(&listings, &today);
    if print_only {
        print!("{text}");
        return Ok(());
    }
    if let Some(parent) = config_path.parent() {
        paths::create_private_dir(parent)?;
    }
    let mut file = std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(config_path)
        .with_context(|| format!("cannot create {}", paths::display(config_path)))?;
    file.write_all(text.as_bytes())?;

    let stubs: usize = listings.iter().map(|l| l.visible.len()).sum();
    println!(
        "Wrote {} with {stubs} grant stubs, one per top-level directory.",
        paths::display(config_path)
    );
    println!(
        "Next: edit it to define your grants, check it with `stomp doctor`, then run `stomp usage`."
    );
    Ok(())
}

fn list_root(root: &Path, backend: &dyn Backend) -> Result<RootListing> {
    let dev = std::fs::symlink_metadata(root)?.dev();
    let (entries, _) = read_entries(root)?;
    let mut loose = Usage::default();
    let mut dirs = Vec::new();
    let mut unnamed = 0;
    for entry in entries {
        if entry.is_dir_on(dev) {
            match entry.path.file_name().and_then(|n| n.to_str()) {
                Some(name) => dirs.push((name.to_string(), entry.path)),
                None => unnamed += 1, // not valid UTF-8, so it can't be written in TOML
            }
        } else if !entry.md.is_dir() {
            loose.add(&Usage::file(&entry.md));
        }
    }
    let instant = backend.is_instant();
    let mut infos: Vec<DirInfo> = dirs
        .par_iter()
        .map(|(name, path)| DirInfo {
            name: name.clone(),
            display: paths::display(path),
            usage: if instant {
                backend.usage(path).ok()
            } else {
                None
            },
        })
        .collect();
    let bytes = |d: &DirInfo| d.usage.as_ref().map_or(0, |u| u.bytes);
    infos.sort_by(|a, b| bytes(b).cmp(&bytes(a)).then_with(|| a.name.cmp(&b.name)));
    let (hidden, visible) = infos.into_iter().partition(|d| d.name.starts_with('.'));
    Ok(RootListing {
        display: paths::display(root),
        backend: backend.kind(),
        visible,
        hidden,
        loose,
        unnamed,
    })
}

/// A TOML string literal.
fn quote(s: &str) -> String {
    toml::Value::String(s.to_string()).to_string()
}

/// A grant ID suggestion derived from a directory name.
fn suggest_id(name: &str) -> String {
    let cleaned: String = name
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.') {
                c.to_ascii_uppercase()
            } else {
                '-'
            }
        })
        .collect();
    let mut id: String = cleaned
        .trim_start_matches(['.', '-'])
        .chars()
        .take(58)
        .collect();
    if id.is_empty() {
        id = "GRANT".to_string();
    } else if RESERVED_GRANT_IDS
        .iter()
        .any(|r| r.eq_ignore_ascii_case(&id))
    {
        id = format!("GRANT-{id}");
    }
    debug_assert!(valid_grant_id(&id), "{id}");
    id
}

fn describe(d: &DirInfo) -> String {
    match &d.usage {
        Some(u) => format!(
            "{}, {} files, last change {}",
            format_bytes(u.bytes),
            format_count(u.files),
            age(u.newest_change)
        ),
        None => "size not measured (not on CephFS)".to_string(),
    }
}

fn render(listings: &[RootListing], today: &str) -> String {
    let rule = format!("# {}", "-".repeat(74));
    let roots: Vec<String> = listings.iter().map(|l| quote(&l.display)).collect();
    let mut lines: Vec<String> = vec![
        format!("# stomp configuration, generated by `stomp init` on {today}."),
        "#".into(),
        "# Each [[grant]] block claims one or more directories, and every file under".into(),
        "# them counts toward that grant. A directory can belong to only one grant.".into(),
        "# Optional grant fields: title, pi, start and end (dates such as 2027-06-30).".into(),
        "#".into(),
        "# Check this file with `stomp doctor`, then run `stomp usage`.".into(),
        String::new(),
        "[general]".into(),
        format!("roots = [{}]", roots.join(", ")),
        String::new(),
        "# Settings for the Slurm tasks stomp will submit for scans and archiving.".into(),
        "# [slurm]".into(),
        "# account   = \"myaccount\"".into(),
        "# partition = \"standard\"".into(),
        "# time      = \"12:00:00\"".into(),
        "# cpus      = 8".into(),
        "# mem       = \"16G\"".into(),
    ];
    for listing in listings {
        let order = match listing.backend {
            BackendKind::Ceph => ", largest first",
            BackendKind::Posix => "",
        };
        lines.extend([
            String::new(),
            rule.clone(),
            format!(
                "# Directories in {} ({}){order}.",
                listing.display,
                listing.backend.label()
            ),
            "# Uncomment and edit a block for each grant. A grant with several".into(),
            "# directories lists them all in `dirs`.".into(),
            rule.clone(),
        ]);
        for d in &listing.visible {
            lines.extend([
                String::new(),
                format!("# {}: {}", d.display, describe(d)),
                "# [[grant]]".into(),
                format!("# id    = {}", quote(&suggest_id(&d.name))),
                format!("# title = {}", quote(&d.name)),
                format!("# dirs  = [{}]", quote(&d.display)),
            ]);
        }
        if !listing.hidden.is_empty() {
            lines.extend([
                String::new(),
                format!(
                    "# Hidden directories in {}. These usually hold settings, caches and",
                    listing.display
                ),
                "# software environments, and count as unassigned unless a grant claims them:"
                    .into(),
            ]);
            for d in &listing.hidden {
                lines.push(format!("#   {}: {}", d.display, describe(d)));
            }
        }
        if listing.loose.files > 0 {
            let noun = if listing.loose.files == 1 {
                "file"
            } else {
                "files"
            };
            lines.extend([
                String::new(),
                format!(
                    "# Files directly in {}: {} in {} {noun} (unassigned).",
                    listing.display,
                    format_bytes(listing.loose.bytes),
                    format_count(listing.loose.files)
                ),
            ]);
        }
        if listing.unnamed > 0 {
            lines.push(format!(
                "# {} directories with names that aren't valid UTF-8 were left out.",
                listing.unnamed
            ));
        }
    }
    lines.push(String::new());
    lines.join("\n")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::Config;

    #[test]
    fn suggests_valid_ids() {
        assert_eq!(suggest_id("carbon_mapping"), "CARBON_MAPPING");
        assert_eq!(suggest_id("my project (old)"), "MY-PROJECT--OLD-");
        assert_eq!(suggest_id(".hidden"), "HIDDEN");
        assert_eq!(suggest_id("stomp"), "GRANT-STOMP");
        assert_eq!(suggest_id("é"), "GRANT");
    }

    /// The generated file must be valid as written, and still valid once a stub is uncommented.
    #[test]
    fn rendered_config_parses_before_and_after_uncommenting() {
        let tmp = tempfile::tempdir().unwrap();
        let root = std::fs::canonicalize(tmp.path()).unwrap();
        std::fs::create_dir(root.join("odd \"name\"")).unwrap();
        std::fs::create_dir(root.join(".cache")).unwrap();
        std::fs::write(root.join("loose.txt"), "x").unwrap();
        let listing = list_root(&root, &crate::fs::posix::PosixBackend).unwrap();
        let text = render(&[listing], "2026-11-16");

        Config::parse(&text, Path::new("init.toml")).unwrap();

        let uncommented = text
            .lines()
            .map(|l| {
                let body = l.strip_prefix("# ").unwrap_or(l);
                if ["[[grant]]", "id ", "title ", "dirs "]
                    .iter()
                    .any(|p| body.starts_with(p))
                {
                    body
                } else {
                    l
                }
            })
            .collect::<Vec<_>>()
            .join("\n");
        let cfg = Config::parse(&uncommented, Path::new("init.toml")).unwrap();
        assert_eq!(cfg.grants.len(), 1);
        assert_eq!(cfg.grants[0].dirs[0].path, root.join("odd \"name\""));
        assert!(text.contains("Hidden directories"));
        assert!(text.contains("Files directly in"));
    }
}
