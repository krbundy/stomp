//! SFTP destinations (§13.2), such as a NAS, through the system's OpenSSH
//! `sftp` and `ssh`, so the user's `~/.ssh/config`, keys and agent all work.
//!
//! Files are uploaded under a `.partial` name, verified, then renamed. How a
//! file is verified depends on what the server allows: `sha256sum` over SSH
//! when it permits commands, otherwise reading the file back over SFTP.

use std::io::Write;
use std::path::Path;
use std::process::{Command, Stdio};

use anyhow::{Context, Result, bail};

use super::{Check, Destination};
use crate::archive::manifest::sha256_file;
use crate::config::{SftpDestination, SftpVerify};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Mode {
    /// `sha256sum` over SSH if the server allows it, otherwise read back.
    Auto,
    RemoteSha256,
    Readback,
    Size,
}

pub struct Sftp {
    name: String,
    host: String,
    base: String,
    mode: Mode,
    ssh: String,
    sftp: String,
}

/// Quotes a path for an sftp batch file, escaping glob characters too.
fn quote(path: &str) -> String {
    let mut out = String::from("\"");
    for c in path.chars() {
        if matches!(c, '"' | '\\' | '*' | '?' | '[' | ']') {
            out.push('\\');
        }
        out.push(c);
    }
    out.push('"');
    out
}

/// Quotes a word for the remote shell.
fn shell_quote(s: &str) -> String {
    format!("'{}'", s.replace('\'', r"'\''"))
}

fn parent(path: &str) -> &str {
    path.rsplit_once('/').map_or("", |(p, _)| p)
}

impl Sftp {
    pub fn new(cfg: &SftpDestination) -> Result<Sftp> {
        let base = crate::paths::expand(&cfg.path)?
            .to_string_lossy()
            .trim_end_matches('/')
            .to_string();
        Ok(Sftp {
            name: cfg.name.clone(),
            host: cfg.host.clone(),
            base,
            mode: match cfg.verify {
                None => Mode::Auto,
                Some(SftpVerify::RemoteSha256) => Mode::RemoteSha256,
                Some(SftpVerify::Readback) => Mode::Readback,
                Some(SftpVerify::Size) => Mode::Size,
            },
            ssh: "ssh".into(),
            sftp: "sftp".into(),
        })
    }

    /// Uses other programs instead of `ssh` and `sftp`, for tests.
    #[cfg(test)]
    pub fn with_programs(mut self, ssh: &Path, sftp: &Path) -> Self {
        self.ssh = ssh.to_string_lossy().into_owned();
        self.sftp = sftp.to_string_lossy().into_owned();
        self
    }

    fn remote(&self, rel: &str) -> String {
        if rel.is_empty() {
            self.base.clone()
        } else {
            format!("{}/{rel}", self.base)
        }
    }

    /// Runs sftp commands in batch mode and returns what it printed.
    fn batch(&self, commands: &[String]) -> Result<String> {
        let mut child = Command::new(&self.sftp)
            .args(["-b", "-", "-o", "BatchMode=yes", "-o", "ConnectTimeout=30"])
            .arg(&self.host)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .with_context(|| format!("cannot run {}", self.sftp))?;
        {
            let mut stdin = child.stdin.take().context("no stdin for sftp")?;
            for c in commands {
                writeln!(stdin, "{c}")?;
            }
        }
        let out = child.wait_with_output()?;
        if !out.status.success() {
            bail!(
                "sftp to {} failed: {}",
                self.host,
                String::from_utf8_lossy(&out.stderr).trim()
            );
        }
        Ok(String::from_utf8_lossy(&out.stdout).into_owned())
    }

    /// Commands creating every directory down to `dir`; existing ones are fine.
    fn mkdirs(&self, dir: &str) -> Vec<String> {
        let mut out = Vec::new();
        let mut path = String::new();
        for (i, part) in dir.split('/').enumerate() {
            if i > 0 || dir.starts_with('/') {
                path.push('/');
            }
            path.push_str(part);
            if !part.is_empty() {
                out.push(format!("-mkdir {}", quote(&path)));
            }
        }
        out
    }

    /// `sha256sum` of a remote file over SSH; fails if the server allows no commands.
    fn remote_sha256(&self, path: &str) -> Result<String> {
        let out = Command::new(&self.ssh)
            .args(["-o", "BatchMode=yes", "-o", "ConnectTimeout=30"])
            .arg(&self.host)
            .arg("--")
            .arg(format!("sha256sum -- {}", shell_quote(path)))
            .stdin(Stdio::null())
            .output()
            .with_context(|| format!("cannot run {}", self.ssh))?;
        if !out.status.success() {
            bail!(
                "running sha256sum on {} failed: {}",
                self.host,
                String::from_utf8_lossy(&out.stderr).trim()
            );
        }
        let text = String::from_utf8_lossy(&out.stdout);
        let sum = text.split_whitespace().next().unwrap_or_default();
        if sum.len() != 64 {
            bail!("unexpected sha256sum output from {}: {text:?}", self.host);
        }
        Ok(sum.to_string())
    }

    fn readback(&self, path: &str, scratch: &Path) -> Result<String> {
        let tmp = scratch.join(format!(".stomp-readback-{}", std::process::id()));
        let result = self
            .batch(&[format!(
                "get {} {}",
                quote(path),
                quote(&tmp.to_string_lossy())
            )])
            .and_then(|_| sha256_file(&tmp));
        let _ = std::fs::remove_file(&tmp);
        result
    }

    fn size_of(&self, path: &str) -> Result<u64> {
        let out = self.batch(&[format!("ls -ln {}", quote(path))])?;
        out.lines()
            .filter(|l| !l.starts_with("sftp>"))
            .find_map(|l| l.split_whitespace().nth(4)?.parse().ok())
            .with_context(|| format!("couldn't read the size of {path} on {}", self.host))
    }

    fn check_file(&self, path: &str, sha256: &str, size: u64, scratch: &Path) -> Result<()> {
        let actual = match self.mode {
            Mode::Size => {
                let stored = self.size_of(path)?;
                if stored != size {
                    bail!("{path} on {} is {stored} bytes, expected {size}", self.host);
                }
                return Ok(());
            }
            Mode::RemoteSha256 => self.remote_sha256(path)?,
            Mode::Readback => self.readback(path, scratch)?,
            Mode::Auto => match self.remote_sha256(path) {
                Ok(sum) => sum,
                Err(_) => self.readback(path, scratch)?,
            },
        };
        if actual != sha256 {
            bail!("{path} on {} has the wrong checksum", self.host);
        }
        Ok(())
    }
}

impl Destination for Sftp {
    fn name(&self) -> &str {
        &self.name
    }

    fn kind(&self) -> &'static str {
        "sftp"
    }

    fn location(&self, rel: &str) -> String {
        format!("{}:{}", self.host, self.remote(rel))
    }

    fn put(&self, local: &Path, rel: &str, sha256: &str, scratch: &Path) -> Result<()> {
        let target = self.remote(rel);
        let partial = format!("{target}.partial");
        let mut commands = self.mkdirs(parent(&target));
        commands.push(format!(
            "put {} {}",
            quote(&local.to_string_lossy()),
            quote(&partial)
        ));
        self.batch(&commands)?;
        let size = std::fs::metadata(local)?.len();
        self.check_file(&partial, sha256, size, scratch)?;
        self.batch(&[
            format!("-rm {}", quote(&target)),
            format!("rename {} {}", quote(&partial), quote(&target)),
        ])?;
        Ok(())
    }

    fn verify(&self, rel: &str, sha256: &str, size: u64, scratch: &Path) -> Result<()> {
        self.check_file(&self.remote(rel), sha256, size, scratch)
    }

    fn get(&self, rel: &str, local: &Path) -> Result<()> {
        self.batch(&[format!(
            "get {} {}",
            quote(&self.remote(rel)),
            quote(&local.to_string_lossy())
        )])?;
        Ok(())
    }

    fn delete(&self, rel: &str) -> Result<()> {
        self.batch(&[format!("-rm {}", quote(&self.remote(rel)))])?;
        Ok(())
    }

    fn delete_dir(&self, rel: &str) -> Result<()> {
        self.batch(&[format!("-rmdir {}", quote(&self.remote(rel)))])?;
        Ok(())
    }

    fn list(&self, rel: &str) -> Result<Vec<(String, u64)>> {
        // `-ls`: a directory that doesn't exist lists as empty instead of failing.
        let out = self.batch(&[format!("-ls -ln {}", quote(&self.remote(rel)))])?;
        Ok(out
            .lines()
            .filter(|l| !l.starts_with("sftp>"))
            .filter_map(|l| {
                let fields: Vec<&str> = l.split_whitespace().collect();
                let size = fields.get(4)?.parse().ok()?;
                let name = fields.last()?.rsplit('/').next()?.to_string();
                Some((name, size))
            })
            .collect())
    }

    fn check(&self) -> Vec<Check> {
        let mut out = Vec::new();
        if let Err(e) = self.batch(&["pwd".into()]) {
            out.push(Check::Fail(format!(
                "can't log in to {} without a password prompt: {e:#}. Check that `sftp {}` works with an SSH key.",
                self.host, self.host
            )));
            return out;
        }
        out.push(Check::Pass(format!(
            "can log in to {} over SFTP",
            self.host
        )));

        let scratch = std::env::temp_dir();
        let test = scratch.join(format!(".stomp-doctor-{}", std::process::id()));
        let data = b"stomp doctor test file";
        let sha = super::sigv4::sha256_hex(data);
        if std::fs::write(&test, data).is_err() {
            out.push(Check::Warn("couldn't create a local test file".into()));
            return out;
        }
        let rel = format!(".stomp-doctor-{}", std::process::id());
        let remote = self.remote(&rel);
        let mut commands = self.mkdirs(&self.base);
        commands.push(format!(
            "put {} {}",
            quote(&test.to_string_lossy()),
            quote(&remote)
        ));
        match self.batch(&commands) {
            Ok(_) => out.push(Check::Pass(format!("can write to {}", self.base))),
            Err(e) => {
                out.push(Check::Fail(format!("can't write to {}: {e:#}", self.base)));
                let _ = std::fs::remove_file(&test);
                return out;
            }
        }
        match self.remote_sha256(&remote) {
            Ok(sum) if sum == sha => out.push(Check::Pass(
                "can run sha256sum over SSH, so uploads are verified on the server".into(),
            )),
            _ if self.mode == Mode::RemoteSha256 => out.push(Check::Fail(
                "verify = \"remote-sha256\", but the server won't run sha256sum over SSH; use \"readback\"".into(),
            )),
            _ => match self.readback(&remote, &scratch) {
                Ok(sum) if sum == sha => out.push(Check::Info(
                    "the server allows SFTP only, so uploads are verified by reading them back (twice the transfer)".into(),
                )),
                Ok(_) => out.push(Check::Fail("a test file read back with the wrong content".into())),
                Err(e) => out.push(Check::Fail(format!("can't read files back: {e:#}"))),
            },
        }
        let _ = self.delete(&rel);
        let _ = std::fs::remove_file(&test);
        out
    }
}

#[cfg(test)]
mod tests;
