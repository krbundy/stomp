//! The SFTP destination against stand-in `sftp` and `ssh` programs that act on
//! a local directory, one allowing shell commands and one SFTP-only. They
//! check the batch commands stomp sends and how it reads the replies; real
//! OpenSSH still needs checking against a real server.

use std::path::{Path, PathBuf};

use super::*;
use crate::archive::manifest::sha256_file;

const FAKE_SFTP: &str = r#"#!/usr/bin/env python3
import os, shlex, shutil, sys
ROOT = __ROOT__
def remote(p):
    return ROOT + p
for raw in sys.stdin:
    line = raw.strip()
    if not line:
        continue
    print("sftp> " + line)
    ignore = line.startswith("-")
    words = shlex.split(line.lstrip("-"))
    cmd, args = words[0], words[1:]
    try:
        if cmd == "pwd":
            print("Remote working directory: /")
        elif cmd == "mkdir":
            os.mkdir(remote(args[0]))
        elif cmd == "put":
            shutil.copyfile(args[0], remote(args[1]))
        elif cmd == "get":
            shutil.copyfile(remote(args[0]), args[1])
        elif cmd == "rename":
            if os.path.exists(remote(args[1])):
                raise OSError("exists")
            os.rename(remote(args[0]), remote(args[1]))
        elif cmd == "rm":
            os.remove(remote(args[0]))
        elif cmd == "rmdir":
            os.rmdir(remote(args[0]))
        elif cmd == "ls":
            p = args[-1]
            names = [p + "/" + n for n in sorted(os.listdir(remote(p)))] if os.path.isdir(remote(p)) else [p]
            for n in names:
                print("-rw-r--r--    ? 1000     1000 %12d Nov 17 11:04 %s" % (os.path.getsize(remote(n)), n))
        else:
            raise OSError("unknown command " + cmd)
    except OSError as e:
        if not ignore:
            print("sftp error: %s" % e, file=sys.stderr)
            sys.exit(1)
"#;

const FAKE_SSH: &str = r#"#!/usr/bin/env python3
import hashlib, shlex, sys
ROOT = __ROOT__
if not __SHELL__:
    print("This service allows sftp connections only.", file=sys.stderr)
    sys.exit(1)
words = shlex.split(sys.argv[-1])
if words[0] != "sha256sum":
    sys.exit(127)
path = words[-1]
with open(ROOT + path, "rb") as f:
    print(hashlib.sha256(f.read()).hexdigest() + "  " + path)
"#;

struct Fake {
    _tmp: tempfile::TempDir,
    dir: PathBuf,
    root: PathBuf,
}

fn python() -> bool {
    Command::new("python3")
        .arg("--version")
        .output()
        .is_ok_and(|o| o.status.success())
}

fn fake(shell: bool) -> Fake {
    let tmp = tempfile::tempdir().unwrap();
    let dir = tmp.path().to_path_buf();
    let root = dir.join("server");
    std::fs::create_dir_all(&root).unwrap();
    let quoted = format!("{:?}", root.to_string_lossy());
    for (name, text) in [("sftp", FAKE_SFTP), ("ssh", FAKE_SSH)] {
        let path = dir.join(name);
        let body = text
            .replace("__ROOT__", &quoted)
            .replace("__SHELL__", if shell { "True" } else { "False" });
        std::fs::write(&path, body).unwrap();
        std::fs::set_permissions(&path, std::os::unix::fs::PermissionsExt::from_mode(0o755))
            .unwrap();
    }
    Fake {
        _tmp: tmp,
        dir,
        root,
    }
}

fn sftp(f: &Fake, verify: Option<SftpVerify>) -> Sftp {
    let cfg = SftpDestination {
        name: "nas".into(),
        host: "nas.example".into(),
        path: "/archive/u".into(),
        parallel_uploads: None,
        verify,
    };
    Sftp::new(&cfg)
        .unwrap()
        .with_programs(&f.dir.join("ssh"), &f.dir.join("sftp"))
}

fn local(dir: &Path, len: usize) -> (PathBuf, String) {
    let path = dir.join("chunk with space.bin");
    std::fs::write(&path, (0..len).map(|i| i as u8).collect::<Vec<_>>()).unwrap();
    let sha = sha256_file(&path).unwrap();
    (path, sha)
}

#[test]
fn quoting() {
    assert_eq!(quote(r#"a "b" [c]*"#), r#""a \"b\" \[c\]\*""#);
    assert_eq!(shell_quote("it's"), r#"'it'\''s'"#);
}

#[test]
fn round_trip_with_shell_access() {
    if !python() {
        return;
    }
    let f = fake(true);
    let nas = sftp(&f, None);
    let (file, sha) = local(&f.dir, 5000);
    nas.put(&file, "G/A/chunk-0001.tar.zst", &sha, &f.dir)
        .unwrap();
    let stored = f.root.join("archive/u/G/A/chunk-0001.tar.zst");
    assert!(stored.exists());
    assert!(
        !f.root
            .join("archive/u/G/A/chunk-0001.tar.zst.partial")
            .exists()
    );
    assert_eq!(nas.location("G/A/x"), "nas.example:/archive/u/G/A/x");
    assert_eq!(
        nas.list("G/A").unwrap(),
        [("chunk-0001.tar.zst".to_string(), 5000)]
    );

    let back = f.dir.join("back");
    nas.get("G/A/chunk-0001.tar.zst", &back).unwrap();
    assert_eq!(sha256_file(&back).unwrap(), sha);

    // Damage on the server is caught.
    let mut data = std::fs::read(&stored).unwrap();
    data[0] ^= 1;
    std::fs::write(&stored, data).unwrap();
    assert!(
        nas.verify("G/A/chunk-0001.tar.zst", &sha, 5000, &f.dir)
            .is_err()
    );

    nas.delete("G/A/chunk-0001.tar.zst").unwrap();
    nas.delete_dir("G/A").unwrap();
    assert!(!f.root.join("archive/u/G/A").exists());

    let checks = nas.check();
    assert!(
        checks
            .iter()
            .any(|c| matches!(c, Check::Pass(t) if t.contains("sha256sum"))),
        "{checks:?}"
    );
}

#[test]
fn sftp_only_server_is_verified_by_reading_back() {
    if !python() {
        return;
    }
    let f = fake(false);
    let (file, sha) = local(&f.dir, 3000);
    sftp(&f, None).put(&file, "G/A/c", &sha, &f.dir).unwrap();
    // Asking for sha256sum explicitly fails on such a server.
    assert!(
        sftp(&f, Some(SftpVerify::RemoteSha256))
            .verify("G/A/c", &sha, 3000, &f.dir)
            .is_err()
    );
    sftp(&f, Some(SftpVerify::Size))
        .verify("G/A/c", &sha, 3000, &f.dir)
        .unwrap();
    let checks = sftp(&f, None).check();
    assert!(
        checks
            .iter()
            .any(|c| matches!(c, Check::Info(t) if t.contains("SFTP only"))),
        "{checks:?}"
    );
    let checks = sftp(&f, Some(SftpVerify::RemoteSha256)).check();
    assert!(
        checks.iter().any(|c| matches!(c, Check::Fail(_))),
        "{checks:?}"
    );
}

#[test]
fn home_relative_paths_mean_the_servers_home() {
    let user = std::env::var("USER").unwrap_or_default();
    // `~` is the home directory on the server: a path relative to where sftp logs in.
    assert_eq!(
        super::remote_base("~/archive/$USER/").unwrap(),
        format!("archive/{user}")
    );
    assert_eq!(super::remote_base("~").unwrap(), ".");
    assert_eq!(
        super::remote_base("/archive/data/").unwrap(),
        "/archive/data"
    );
    assert!(super::remote_base("~/x/$NO_SUCH_VARIABLE_HERE").is_err());
}
