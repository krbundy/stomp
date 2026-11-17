# stomp

A storage management utility for HPC users. stomp runs as you, with no root or
admin help, and attributes your storage and Slurm jobs to the research grants
that pay for them.

The full design is in [`spec/design.md`](spec/design.md). Milestones M1, M2
and M3 are done:

| Command | What it does |
|---------|--------------|
| `stomp init` | Writes a starter config listing your top-level directories |
| `stomp doctor` | Checks the config and environment |
| `stomp usage` | Storage per grant, instant on CephFS; each run is saved to the history |
| `stomp du [DIR] [-d N]` | Directory sizes with the grant each one belongs to |
| `stomp scan` | Submits a Slurm task that scans in detail: data ages, file types, environments, caches |
| `stomp tasks` | Shows stomp's Slurm tasks; `show ID` and `cancel ID` |
| `stomp changes` | How each grant's storage changed; `--dirs` compares two scans directory by directory |
| `stomp jobs log GRANT` | Records that a Slurm job belongs to a grant |
| `stomp jobs list` | Recent jobs and their grant; `retag` and `untag` fix mistakes |
| `stomp report compute` | CPU and GPU hours per grant (`--by-month`) |
| `stomp report grant ID` | One grant's storage and compute together |
| `stomp report history` | Storage per grant over time, with TB-months |
| `stomp report stale` | Big directories nothing has touched in 180 days |
| `stomp report reclaimable` | Caches and environments, with commands to slim them down |
| `stomp report lifecycle` | Grants that have ended or are about to, and what they still use |
| `stomp archive DIR` | Packs a directory into compressed, verified chunks and removes the originals |
| `stomp archives` | Lists archives; `show` and `verify [--deep]` |
| `stomp restore ID` | Puts archived files back, all of them or `--only` some |

Data commands accept `--format table|csv|json|md`.

### Scans

`stomp scan` runs as a Slurm task using the `[slurm]` settings in your config;
override them per run with `--time`, `--cpus`, `--mem`, `--partition` and
`--account`. If the task reaches its time limit, it saves what it finished and
resubmits itself to continue. `--here` runs the scan in the foreground instead.

On CephFS, scans after the first skip directories that haven't changed. A full
scan runs every 30 days (`scan.full_every`), or on request with `--full`.

Software environments (Apptainer sandboxes, conda environments, Python venvs),
known caches, and anything matching `scan.exclude` are counted toward their
grant but not scanned.

### Archives

```sh
stomp archive ~/project/runs_2023 --estimate   # the plan and expected size; changes nothing
stomp archive ~/project/runs_2023              # asks, then submits a Slurm task
stomp archives                                 # list them
stomp restore <ID>                             # everything back where it was
```

The archive stays inside the directory, in `.stomp-archive/<ID>`, next to a
`STOMP_ARCHIVED.txt` note that explains what happened. Files are packed into
`tar.zst` chunks (50 GiB by default). Each chunk is read back and every file's
checksum compared before any original is removed, and an original is only
removed if it hasn't changed since it was packed. An interrupted archive
continues with `stomp archive --resume <DIR>`. Environments, caches and files
owned by other people stay in place.

Every archive includes a `README.txt` explaining how to restore it with just
`sha256sum`, `zstd` and `tar`. The archive never depends on stomp.

## Building

```sh
cargo build --release              # target/release/stomp
```

For a static binary that runs on any Linux cluster regardless of its glibc
version (needs `musl-tools` for the bundled SQLite):

```sh
rustup target add x86_64-unknown-linux-musl
cargo build --release --target x86_64-unknown-linux-musl
```

Copy the binary to `~/bin` on the cluster.

## Getting started

```sh
stomp init          # writes ~/.config/stomp/config.toml
$EDITOR ~/.config/stomp/config.toml
stomp doctor
stomp usage
```

A grant claims one or more directories, and everything under them counts
toward it:

```toml
[[grant]]
id    = "GRANT-A"
title = "Example grant"
end   = 2027-06-30
dirs  = ["~/project_alpha", "~/project_alpha_runs"]
```

To attribute compute jobs, add one line to a job script:

```sh
#!/bin/bash
#SBATCH --time=04:00:00
stomp jobs log GRANT-A
srun ./model
```

`stomp jobs log` never makes a job fail. Problems are printed as warnings and
it exits 0, unless you pass `--strict`.

Most jobs don't even need that line. A job that runs from inside a grant's
directory is attributed to that grant automatically. CPU and GPU hours come
from Slurm accounting (`sacct`), which stomp imports as needed.
