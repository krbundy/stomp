//! Editing the config from the TUI: adding a directory to a grant or removing
//! it, keeping the file's comments and layout. Every edit is checked by
//! loading the result as a config before anything is written, and the
//! previous file is kept as `config.toml.bak`.

use std::path::{Path, PathBuf};

use anyhow::{Context, Result, bail};
use toml_edit::{Array, DocumentMut, Item, Value};

use crate::config::Config;
use crate::paths;

/// Whether a `dirs` entry from the config names `path`.
fn names(entry: &str, path: &Path) -> bool {
    paths::expand(entry).is_ok_and(|p| paths::resolve(&p).0 == path)
}

fn parse(text: &str) -> Result<DocumentMut> {
    text.parse::<DocumentMut>()
        .context("the config file isn't valid TOML")
}

/// Adds `entry` (as written in the file, e.g. `~/runs`) to the grant's `dirs`.
pub fn assign(text: &str, grant: &str, entry: &str) -> Result<String> {
    let mut doc = parse(text)?;
    let Some(grants) = doc.get_mut("grant").and_then(Item::as_array_of_tables_mut) else {
        bail!("the config has no [[grant]] blocks");
    };
    let Some(table) = grants
        .iter_mut()
        .find(|t| t.get("id").and_then(Item::as_str) == Some(grant))
    else {
        bail!("there's no [[grant]] block with id = {grant:?}");
    };
    match table.get_mut("dirs") {
        None => {
            let mut dirs = Array::new();
            dirs.push(entry);
            table.insert("dirs", Item::Value(Value::Array(dirs)));
        }
        Some(item) => {
            let Some(dirs) = item.as_array_mut() else {
                bail!("grant {grant}: dirs isn't a list");
            };
            if dirs.iter().any(|v| v.as_str() == Some(entry)) {
                bail!("{entry} is already one of grant {grant}'s directories");
            }
            dirs.push(entry);
        }
    }
    Ok(doc.to_string())
}

/// Removes the `dirs` entry naming `path` from whichever grant lists it.
/// Returns the new text and the grant.
pub fn unassign(text: &str, path: &Path) -> Result<(String, String)> {
    let mut doc = parse(text)?;
    let Some(grants) = doc.get_mut("grant").and_then(Item::as_array_of_tables_mut) else {
        bail!("the config has no [[grant]] blocks");
    };
    let mut found = None;
    for table in grants.iter_mut() {
        let id = table
            .get("id")
            .and_then(Item::as_str)
            .unwrap_or_default()
            .to_string();
        let Some(dirs) = table.get_mut("dirs").and_then(Item::as_array_mut) else {
            continue;
        };
        let at = dirs
            .iter()
            .position(|v| v.as_str().is_some_and(|e| names(e, path)));
        if let Some(i) = at {
            dirs.remove(i);
            found = Some(id);
            break;
        }
    }
    if let Some(id) = found {
        return Ok((doc.to_string(), id));
    }
    bail!(
        "{} isn't listed as a grant directory in the config",
        paths::display(path)
    )
}

/// Checks the edited text as a config, then replaces the file, keeping the
/// old one as `<file>.bak`. Returns the new config and the backup's path.
pub fn save(config_path: &Path, text: &str) -> Result<(Config, PathBuf)> {
    let cfg = Config::parse(text, config_path).map_err(|e| {
        anyhow::anyhow!(
            "the change wasn't saved, because the config would have problems:\n  {}",
            e.problems.join("\n  ")
        )
    })?;
    let backup = PathBuf::from(format!("{}.bak", config_path.display()));
    std::fs::copy(config_path, &backup)
        .with_context(|| format!("cannot back up {}", paths::display(config_path)))?;
    let tmp = PathBuf::from(format!("{}.tmp", config_path.display()));
    std::fs::write(&tmp, text)?;
    std::fs::rename(&tmp, config_path)?;
    Ok((cfg, backup))
}

#[cfg(test)]
mod tests {
    use super::*;

    const TEXT: &str = r#"# my config
[general]
roots = ["/r"]

[[grant]]
id   = "A"    # first grant
dirs = ["/r/a"]

[[grant]]
id = "B"
# no dirs yet
"#;

    #[test]
    fn adds_and_removes_directories_keeping_comments() {
        let added = assign(TEXT, "A", "/r/a2").unwrap();
        assert!(added.contains(r#"dirs = ["/r/a", "/r/a2"]"#), "{added}");
        assert!(added.contains("# first grant") && added.contains("# my config"));

        let added = assign(TEXT, "B", "/r/b").unwrap();
        assert!(added.contains("# no dirs yet"));
        let doc = added.parse::<DocumentMut>().unwrap();
        let b = &doc["grant"].as_array_of_tables().unwrap().get(1).unwrap()["dirs"];
        assert_eq!(b.as_array().unwrap().get(0).unwrap().as_str(), Some("/r/b"));

        assert!(assign(TEXT, "A", "/r/a").is_err());
        assert!(assign(TEXT, "Z", "/r/z").is_err());

        let (removed, grant) = unassign(TEXT, Path::new("/r/a")).unwrap();
        assert_eq!(grant, "A");
        assert!(removed.contains("dirs = []"), "{removed}");
        assert!(unassign(TEXT, Path::new("/r/elsewhere")).is_err());
    }

    #[test]
    fn saves_only_valid_configs_and_keeps_a_backup() {
        let dir = tempfile::tempdir().unwrap();
        let root = std::fs::canonicalize(dir.path()).unwrap();
        std::fs::create_dir_all(root.join("a/inner")).unwrap();
        let path = root.join("config.toml");
        let text = TEXT.replace("/r", root.to_str().unwrap());
        std::fs::write(&path, &text).unwrap();

        // B can't claim a directory inside A's.
        let bad = assign(&text, "B", &format!("{}/a/inner", root.display())).unwrap();
        let err = save(&path, &bad).unwrap_err().to_string();
        assert!(err.contains("wasn't saved"), "{err}");
        assert_eq!(std::fs::read_to_string(&path).unwrap(), text);

        let good = assign(&text, "B", &format!("{}/b", root.display())).unwrap();
        let (cfg, backup) = save(&path, &good).unwrap();
        assert_eq!(cfg.grant("B").unwrap().dirs.len(), 1);
        assert_eq!(std::fs::read_to_string(backup).unwrap(), text);
        assert_eq!(std::fs::read_to_string(&path).unwrap(), good);
    }
}
