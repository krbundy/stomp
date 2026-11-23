//! Path expansion and normalization, and stomp's standard locations (§7.1).

use std::env;
use std::io;
use std::os::unix::fs::DirBuilderExt;
use std::path::{Component, Path, PathBuf};
use std::sync::OnceLock;

use anyhow::{Context, Result, bail};

/// The user's home directory, from `$HOME`.
pub fn home_dir() -> Result<PathBuf> {
    match env::var_os("HOME") {
        Some(home) if !home.is_empty() => Ok(PathBuf::from(home)),
        _ => bail!("$HOME is not set"),
    }
}

/// Expands `~`, `$VAR` and `${VAR}` in a path from the config file.
pub fn expand(raw: &str) -> Result<PathBuf> {
    let home = home_dir()?;
    expand_with(raw, &home, |name| env::var(name).ok())
}

/// Like [`expand`], with the home directory and environment supplied by the caller.
pub fn expand_with(
    raw: &str,
    home: &Path,
    var: impl Fn(&str) -> Option<String>,
) -> Result<PathBuf> {
    let mut out = String::new();
    let rest = if raw == "~" {
        push_home(&mut out, home)?;
        ""
    } else if let Some(rest) = raw.strip_prefix("~/") {
        push_home(&mut out, home)?;
        out.push('/');
        rest
    } else if raw.starts_with('~') {
        bail!("{raw:?}: `~user` paths are not supported; write the full path");
    } else {
        raw
    };

    let mut chars = rest.chars().peekable();
    while let Some(c) = chars.next() {
        if c != '$' {
            out.push(c);
            continue;
        }
        let mut name = String::new();
        if chars.peek() == Some(&'{') {
            chars.next();
            loop {
                match chars.next() {
                    Some('}') => break,
                    Some(ch) => name.push(ch),
                    None => bail!("{raw:?}: unterminated `${{`"),
                }
            }
        } else {
            while let Some(&ch) = chars.peek() {
                if ch.is_ascii_alphanumeric() || ch == '_' {
                    name.push(ch);
                    chars.next();
                } else {
                    break;
                }
            }
        }
        if name.is_empty() {
            bail!("{raw:?}: `$` must be followed by a variable name");
        }
        let value = var(&name)
            .with_context(|| format!("{raw:?}: environment variable ${name} is not set"))?;
        out.push_str(&value);
    }
    Ok(PathBuf::from(out))
}

fn push_home(out: &mut String, home: &Path) -> Result<()> {
    out.push_str(home.to_str().context("$HOME is not valid UTF-8")?);
    Ok(())
}

/// Normalizes a path lexically: removes `.` and resolves `..` without touching the filesystem.
pub fn normalize(path: &Path) -> PathBuf {
    let mut out = PathBuf::new();
    for component in path.components() {
        match component {
            Component::RootDir => out.push("/"),
            Component::CurDir | Component::Prefix(_) => {}
            Component::ParentDir => {
                out.pop();
            }
            Component::Normal(part) => out.push(part),
        }
    }
    out
}

/// Resolves a path to its canonical form if it exists, or a normalized form if it doesn't.
/// The flag says whether it exists.
pub fn resolve(path: &Path) -> (PathBuf, bool) {
    match std::fs::canonicalize(path) {
        Ok(canonical) => (canonical, true),
        Err(_) => (normalize(path), false),
    }
}

fn canonical_home() -> Option<&'static Path> {
    static HOME: OnceLock<Option<PathBuf>> = OnceLock::new();
    HOME.get_or_init(|| {
        let home = home_dir().ok()?;
        Some(std::fs::canonicalize(&home).unwrap_or(home))
    })
    .as_deref()
}

/// Formats a path for display, replacing the home directory with `~`.
pub fn display(path: &Path) -> String {
    if let Some(home) = canonical_home()
        && let Ok(rest) = path.strip_prefix(home)
    {
        return if rest.as_os_str().is_empty() {
            "~".to_string()
        } else {
            format!("~/{}", rest.display())
        };
    }
    path.display().to_string()
}

/// The default config file: `$XDG_CONFIG_HOME/stomp/config.toml`.
pub fn config_file() -> Result<PathBuf> {
    Ok(xdg_dir("XDG_CONFIG_HOME", ".config")?
        .join("stomp")
        .join("config.toml"))
}

/// stomp's data directory: `$XDG_DATA_HOME/stomp`.
pub fn data_dir() -> Result<PathBuf> {
    Ok(xdg_dir("XDG_DATA_HOME", ".local/share")?.join("stomp"))
}

fn xdg_dir(var: &str, default_under_home: &str) -> Result<PathBuf> {
    match env::var_os(var) {
        // The XDG spec says relative values must be ignored.
        Some(value) if Path::new(&value).is_absolute() => Ok(PathBuf::from(value)),
        _ => Ok(home_dir()?.join(default_under_home)),
    }
}

/// Creates a directory (and any missing parents) readable only by the user.
pub fn create_private_dir(path: &Path) -> io::Result<()> {
    std::fs::DirBuilder::new()
        .recursive(true)
        .mode(0o700)
        .create(path)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn env(name: &str) -> Option<String> {
        match name {
            "USER" => Some("alice".into()),
            "SCRATCH" => Some("/scratch/alice".into()),
            _ => None,
        }
    }

    #[test]
    fn expands_home_and_variables() {
        let home = Path::new("/home/alice");
        let ex = |raw| expand_with(raw, home, env).unwrap();
        assert_eq!(ex("~"), PathBuf::from("/home/alice"));
        assert_eq!(ex("~/proj"), PathBuf::from("/home/alice/proj"));
        assert_eq!(ex("/archive/$USER"), PathBuf::from("/archive/alice"));
        assert_eq!(ex("/archive/${USER}/x"), PathBuf::from("/archive/alice/x"));
        assert_eq!(ex("$SCRATCH/stomp"), PathBuf::from("/scratch/alice/stomp"));
        assert_eq!(ex("relative/dir"), PathBuf::from("relative/dir"));
    }

    #[test]
    fn rejects_bad_expansions() {
        let home = Path::new("/home/alice");
        assert!(expand_with("~bob/x", home, env).is_err());
        assert!(expand_with("/x/$NOPE", home, env).is_err());
        assert!(expand_with("/x/${USER", home, env).is_err());
        assert!(expand_with("/x/$", home, env).is_err());
    }

    #[test]
    fn normalizes_lexically() {
        assert_eq!(normalize(Path::new("/a/./b/../c/")), PathBuf::from("/a/c"));
        assert_eq!(normalize(Path::new("/../a")), PathBuf::from("/a"));
    }
}
