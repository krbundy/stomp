# stomp

A storage management utility for HPC users. stomp runs as you, with no root or
admin help, and attributes your storage and Slurm jobs to the research grants
that pay for them.

The full design is in [`spec/design.md`](spec/design.md). Milestones M1 through
M5 are done:

| Command | What it does |
|---------|--------------|
| `stomp init` | Writes a starter config listing your top-level directories |
| `stomp doctor` | Checks the config, environment and each destination |
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
| `stomp archive DIR` | Packs a directory into compressed, verified chunks and removes the originals; `--to nas` sends the chunks to a destination |
| `stomp archives` | Lists archives; `show`, `verify [--deep]` and `move --to DEST` |
| `stomp restore ID` | Puts archived files back, all of them or `--only` some, downloading chunks as needed |
| `stomp candidates` | Directories your policies propose to archive, with what archiving would free; `dismiss ID` |
| `stomp apply ID...` | Archives proposed directories, each checked again first |
| `stomp schedule enable` | A small daily Slurm job: usage, accounting, a weekly scan, proposals; `status`, `disable` |

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

### Destinations

Chunks can go to an SFTP server (such as a NAS) or to S3-compatible storage
(such as DigitalOcean Spaces) instead of staying in the directory:

```toml
[[destination]]
name = "nas"
type = "sftp"
host = "nas"                    # anything `sftp` accepts, including ~/.ssh/config aliases
path = "/archive/$USER"

[[destination]]
name = "spaces"
type = "s3"
endpoint = "https://nyc3.digitaloceanspaces.com"
bucket = "lab-archives"
profile = "spaces"              # keys under [spaces] in ~/.aws/credentials
```

```sh
stomp doctor                                   # logs in, writes a test file, checks how uploads are verified
stomp doctor --submit                          # the same from a compute node
stomp archive ~/project/runs_2023 --to nas     # chunks go to the NAS
stomp archives move <ID> --to spaces           # move an existing archive (or --to local)
stomp restore <ID>                             # downloads what it needs; the remote copy is kept
```

Each chunk is verified at the destination before any original file is
removed: with `sha256sum` over SSH (or by reading it back on SFTP-only
servers), and on S3 by the server checking every request's SHA-256 and MD5.
The archive's records stay in the directory, so `stomp archives show` and
planning a restore work offline. If compute nodes can't reach a destination,
archive locally in a task and then run `stomp archives move <ID> --to nas
--here` on a login node.

### Policies and the daily run

Policies in the config propose directories to archive, based on the newest
scan:

```toml
[[policy]]
name  = "ended-grants"
match = { grant_ended_for = "90d" }           # grant directories 90 days after the grant's end

[[policy]]
name   = "old-small-files"
match  = { unchanged_for = "1y", min_files = 10000, max_avg_file_size = "1MiB" }
action = "apply"                              # archive automatically (default: just propose)
wait   = "14d"                                # ...two weeks after first proposing it
```

Conditions are `grant`, `path`, `grant_ended_for`, `unchanged_for`, `min_bytes`,
`min_files` and `max_avg_file_size`; all of a policy's conditions must hold, and
it proposes the outermost directories that meet them. Only data inside grant
directories is proposed, and nothing hidden, unless a policy names a `path`:
software and reference data can look untouched for years while being read
every day.

```sh
stomp candidates                 # proposals, with the space each would free
stomp candidates dismiss 4       # never propose that one again
stomp apply 2 7                  # archive two of them, as Slurm tasks
stomp schedule enable            # daily at a quiet hour, through scrontab if allowed
stomp schedule status            # how the last run went
```

The daily run records a usage sample, imports job accounting, submits a scan
once a week, updates the proposals, and archives those from `apply` policies
once their wait has passed (at most three a day, each checked again first).

### Trying it on your cluster

`scripts/stomp-test.sbatch` measures what archiving would save on your own
GeoTIFF and CSV files without touching them. It copies a random sample from
the directories you name (5 GB of each type by default), then archives,
verifies, restores and compares only the copies, using a separate stomp
config so nothing shows up in your reports:

```sh
sbatch scripts/stomp-test.sbatch ~/carbon_mapping ~/soil
LEVELS="3 9 19" SAMPLE_GB=10 sbatch scripts/stomp-test.sbatch ~/carbon_mapping
```

Results land in `stomp-test-<job>.out` (with a summary at the end) and
`stomp-test-<job>.tsv`. The settings are described at the top of the script.

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
