+++
title = "Install stomp"
description = "Download one static binary to your cluster, or build it from source."
weight = 10
+++

stomp is a single executable file. It needs no root access, no installer and
no libraries on the cluster: you put it in `~/bin` and run it.

## What you need

- **Linux on x86_64.** The static build runs on any distribution, old or new
  (it's tested on CentOS 7 through Rocky Linux 9), because it doesn't depend
  on the system's C library.
- **Slurm commands** (`sbatch`, `squeue`, `sacct`) for running scans and
  archives as jobs and for compute accounting. Most login nodes have them.
  `scrontab` is used for the daily run if your site allows it.
- **OpenSSH** (`ssh`, `sftp`), only if you send archives to an SFTP server.
- **Nothing else.** SQLite, zstd, TLS and certificates for S3 are built in.

## Get the binary

### Download a release

Each [release](https://github.com/krbundy/stomp/releases) has the static
binary and its SHA-256. On a login node:

```sh
mkdir -p ~/bin && cd ~/bin
base=https://github.com/krbundy/stomp/releases/latest/download
curl -fsSLO $base/stomp-x86_64-linux
curl -fsSLO $base/stomp-x86_64-linux.sha256
sha256sum -c stomp-x86_64-linux.sha256 && mv stomp-x86_64-linux stomp && chmod +x stomp
rm stomp-x86_64-linux.sha256
```

```text
stomp-x86_64-linux: OK
```

The checksum confirms the file arrived intact. `latest` is the newest
release; for a particular version, use `download/v0.1.0` in place of
`latest/download`. If your login nodes can't reach GitHub, download the two
files on your own machine and copy them over with `scp`.

### Or build a static binary on your workstation

On a Linux or macOS machine with [Rust](https://rustup.rs) and Docker (or
`musl-tools` on Ubuntu):

```sh
git clone https://github.com/krbundy/stomp.git
cd stomp
scripts/build-static.sh
```

This builds `dist/stomp-<version>-x86_64-linux`, checks that it has no shared
library dependencies, and writes its SHA-256 next to it. Add `--test` to run
the test suite against the static build first.

Copy it to the cluster:

```sh
ssh <cluster> 'mkdir -p ~/bin'
scp dist/stomp-0.1.0-x86_64-linux <cluster>:bin/stomp
ssh <cluster> 'chmod +x ~/bin/stomp && ~/bin/stomp --version'
```

### Or build it on the cluster

If your cluster's login nodes can reach the internet, you can build stomp in
your home directory with a user-level Rust toolchain (Rust 1.89 or newer):

```sh
curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh   # once
cargo install --git https://github.com/krbundy/stomp.git --root ~
```

`cargo install --root ~` puts the binary in `~/bin/stomp`. Building takes a
few minutes and a fair amount of memory; if your site discourages compiling
on login nodes, do it in an interactive job.

## Put it on your PATH

Most clusters already include `~/bin` in `PATH`. If `stomp --version` says
"command not found", add this to `~/.bashrc`:

```sh
export PATH="$HOME/bin:$PATH"
```

Check that it works:

```sh
stomp --version
```

```text
stomp 0.1.0
```

## Updating

Replace `~/bin/stomp` with the new file: [download the
release](#download-a-release) the same way, and it overwrites the old one.
`stomp --version` shows which one you have. Your config
(`~/.config/stomp/config.toml`) and data (`~/.local/share/stomp/`) stay where
they are, and a newer stomp upgrades its catalog automatically the first time
it opens it. If you use the daily run, the next run picks up the new binary.

## Sharing it with your group

The binary has nothing user-specific in it. To give colleagues one copy,
put it in a group directory they can read, such as
`/project/<lab>/bin/stomp`, and have them add that directory to their `PATH`.
Each person still gets their own config, catalog and archives in their own
home directory.

## Next

Continue with the [quick start](@/getting-started/quick-start.md).
