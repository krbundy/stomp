# stomp: Design Specification

**Status:** Draft v0.8 (2026-11-17). Milestones M1 through M5 and M7 are implemented; M6 (cold storage) is deferred.
**Scope:** Behavior and architecture for the first releases. Open questions are listed in §24.

**Changes since v0.7** (decisions made while building M7, the TUI; details in §19.1):
- **Actions leave the browser** and run the CLI's own commands, with their usual questions and checks (D34).
- **Grant assignment edits the config in place**, keeping comments and layout, checking the result first, and keeping a `.bak` copy (D35).
- **Sizes are measured as you browse** (instant on CephFS, a walk elsewhere); the TUI doesn't read scan data for sizes.

**Changes in v0.7** (decisions made while building M5, automation; details in §16.1 and §14.4):
- **Proposals come only from grant data** unless a policy names a `path`, and never from hidden directories that way either (D29).
- **"Unchanged" means neither modified nor changed** (mtime and ctime), so freshly copied or restored data isn't proposed straight away (D30).
- **One small daily job** does the scheduled work; the weekly scan is a separate task it submits (D31).
- **Automatic archiving waits** (7 days by default) after a directory is first proposed, needs an age condition, starts at most 3 archives a run, and checks each directory again first (D32).
- **Scheduled runs open the catalog briefly**, like any command; long tasks still never do (D33).
- Durations accept years (`1y` = 365 days).

**Changes in v0.6** (decisions made while building M4, remote destinations; details in §13.5):
- **S3 uses a small built-in client** (SigV4 over HTTPS), not the AWS SDK (D25, replacing D13).
- **S3 integrity checks:** each request is signed with its body's SHA-256 and carries a Content-MD5, so the server checks both (D26).
- **SFTP verification:** checked with `sha256sum` over SSH where the server allows it, and by reading the file back otherwise; `doctor` reports which applies (D27).
- **`archives push` folds into `archives move`**, run with `--here` on a login node.
- **What stays local:** a remote archive keeps its records (manifest, checksums, README, journal) in the directory; only the chunks leave (D28).
- **Restoring keeps the remote copy** unless `--delete-remote` is given.

**Changes in v0.5** (decisions made while building M3, local archiving):
- §12.6 records how archiving was built: what's refused, the fixed plan, verification, deletion and restore details.
- **The plan is fixed when archiving starts**, so files created afterwards stay in place (D21).
- **Deletion is two-phase**, and hard links are checked without ctime (D22).
- **Each chunk is fully decompressed and checked before any deletion** (D23).
- **Restores are resumable:** files that already match are counted as restored (D24).

**Changes in v0.4** (decisions made while building M2):
- **Ages are stored as months, not age bands.** Each directory keeps a histogram of files and bytes per modification month; reports turn it into age bands at report time (§9.3, D18).
- **File types and largest files** are recorded only by full scans that weren't interrupted (§9.3).
- **Interrupted scans resume by work unit**: the directories two levels below each root (§9.2).
- **`stomp changes`** compares grants with a recorded measurement by default; `--dirs` compares two scans (§11, D20).
- **Retagging order:** `stomp jobs retag <GRANT> <JOB>...`, matching `jobs log` (§15.1).
- **Markdown output:** reports gained `--format md`.
- **Deferred:** HTML reports, the per-file index (`track_files`), and `jobs log --name` (§22).

**Changes in v0.3:**
- The S3 destination targets DigitalOcean Spaces first (§13.3.7). Q4 is resolved.
- Glacier and Deep Archive (M6) and splitting a job across grants (former Q6) are deferred.

**Changes in v0.2:**
- S3 added as an archive destination; the NAS is now one of several named destinations (§13).
- New: Slurm compute jobs are attributed to grants (§15).
- stomp's own Slurm work is renamed **tasks** (§14), so that **jobs** refers to the user's compute jobs.

---

## 1. Summary

stomp is a command-line tool that runs as an ordinary user and ships as a single file. It helps an HPC user track the resources each of their **research grants** uses. It:

- attributes **storage** usage to grants,
- attributes **Slurm compute jobs** to grants (CPU hours and GPU hours),
- tracks how usage **changes over time** and produces **reports** for grant budgets,
- finds data worth compressing or archiving, using rules the user writes,
- packs that data into **compressed archives**, either kept in place or sent to a **remote destination** (a NAS over SFTP, or S3-compatible object storage),
- and **restores** archives on demand, including partial restores.

Heavy work runs as **Slurm jobs** that stomp submits for the user. stomp needs no root access, no background service and no help from the HPC admins.

## 2. Background

- The HPC center reports usage only per user, not per project or grant. This is true for both storage and compute. Users funded by several grants have to work out each grant's share themselves, for budgets, progress reports and data management plans.
- Home directories collect finished-project data, container sandboxes, package caches and millions of small files. Users have no tooling of their own to compress this data or move it to cheaper storage such as a NAS or S3.
- Existing tools fall short for this user. Starfish, Robinhood, GUFI, archivetar and mpiFileUtils are commercial, run by admins, or cover only one piece. None of them maps usage to a user's own grants, attributes compute jobs, or sends data to SFTP or S3 destinations of the user's choosing.

## 3. Goals and non-goals

### Goals

| ID | Goal |
|----|------|
| G1 | Per-grant storage usage, available instantly wherever the filesystem allows it. |
| G2 | Per-grant compute usage, from explicit job logging and automatic inference. |
| G3 | Usage history and change tracking. |
| G4 | Grant-oriented reports (table, CSV, Markdown, HTML, JSON), including TB-months, CPU hours and GPU hours. |
| G5 | Rule-driven discovery of archive candidates; propose or perform compression. |
| G6 | Safe archive and restore, locally or at remote destinations (SFTP, S3), with partial restores. |
| G7 | Everything runs as the user: no root, no background service, no admin involvement. |
| G8 | Easy for colleagues to adopt: one static binary, and `stomp init` gets a new user going in minutes. |
| G9 | Good citizen on shared systems: heavy work goes to Slurm, and load on the metadata server and the accounting database is limited. |
| G10 | Archives can be restored with standard tools (`tar`, `zstd`, `sha256sum`) even without stomp. |

### Non-goals

- **Not an admin or site-wide tool.** stomp manages only one user's data and jobs.
- **Not a backup system.** Archiving *moves* data: after archiving to a remote destination, that copy is the only copy.
- **Not a sync tool.** There is no two-way mirroring.
- **Not a billing system.** stomp can estimate cost from configured rates but does not invoice anyone.
- **Not a job submission wrapper.** stomp records which grant a job belongs to; it does not change how jobs are submitted.

## 4. Environment and constraints

Known facts about the target cluster as of this draft:

| Item | Observed | Consequence |
|------|----------|-------------|
| `/home` | CephFS (kernel client), ~3.1 PB shared | Ceph's built-in directory totals give fast usage numbers (§6.1). |
| Quotas | `quota -s` and `lfs quota` report nothing | Any quota is exposed as `ceph.quota.*` attributes. |
| Scheduler | Slurm | Heavy commands submit tasks (§14); compute attribution reads `sacct` (§15). |
| NAS | User has previously downloaded from the NAS inside a Slurm job | SFTP transfers can probably run inside tasks. |
| S3 | User keeps some data in DigitalOcean Spaces (S3-compatible) | S3 destination, built and tested against Spaces first (§13.3). |
| Privileges | Ordinary user | No admin-only filesystem or Slurm features. |
| File counts | Very large (tens of millions or more in one directory tree) | No per-file records in the catalog by default (§9.3). |

Constraints:

- **C1.** stomp's own data lives in the user's quota, so the catalog must stay small (§10.3).
- **C2.** The CephFS metadata server and Slurm's accounting database are shared by everyone. Scans must limit how many files they examine in parallel, and `sacct` queries must cover bounded time windows.
- **C3.** Only cheap operations may run on login nodes.
- **C4.** Slurm jobs have wall-time limits, so every long operation must save progress and be able to resume.
- **C5.** SQLite's file locking can't be trusted on a network filesystem, so only one process may write the catalog (§10.2).
- **C6.** Compute nodes may not have outbound internet access. A destination that is reachable from a login node may not be reachable from a task (§13.4).

## 5. Concepts

- **Root:** a top-level directory stomp manages, such as `~` or a project space. A user can have several. Roots may not be nested inside each other.
- **Grant:** a funding source with metadata (ID, title, PI, start and end dates) and one or more grant directories. The grant is the central unit everything is reported against.
- **Grant directory:** a directory whose entire subtree is attributed to one grant. A grant may have several directories; a directory belongs to at most one grant.
- **Unassigned:** storage or compute that can't be attributed to any grant.
- **Excluded directory:** a directory that is counted in usage but never scanned in detail or archived. There are four kinds:
  - *environment:* container sandboxes, conda environments, Python venvs
  - *cache:* known package and container caches
  - *user:* directories matching the user's own glob patterns
  - *stomp:* stomp's own data and staging directories
- **Usage sample:** a timestamped storage measurement of (path, bytes, files).
- **Scan:** a detailed walk of the directory tree that records per-directory totals.
- **Archive:** one directory packed into one or more **chunks** (`.tar.zst` files) plus a manifest. It is stored locally or at a destination.
- **Destination:** a named remote place where archives can be stored, of type `sftp` or `s3` (§13).
- **Policy:** a rule that selects archive candidates and either proposes or applies an action.
- **Proposal:** a candidate archive action produced by a policy and waiting for the user's approval.
- **Compute job:** one of the user's own Slurm jobs. It is attributed to a grant explicitly or by its working directory (§15).
- **Task:** a Slurm job that stomp submits for its own work (scan, archive, restore, transfer) and tracks.

## 6. Filesystem backends

Filesystem access goes through a common interface:

- `dir_usage(path) -> Option<Usage { bytes, files, subdirs, rctime }>`: instant on CephFS; on other filesystems, `None`.
- `quota(path) -> Option<Quota { max_bytes, max_files }>`
- `walk(root, options)`: a parallel directory walk with limited concurrency (§9).

### 6.1 Ceph backend

**Detection.** The `statfs` filesystem type is `0x00c36400` (kernel client), or reading `ceph.dir.rbytes` on the root succeeds (which also covers `ceph-fuse`).

CephFS keeps running totals for every directory and exposes them as virtual extended attributes. Any user can read them with no tree walk:

| Attribute | Meaning |
|-----------|---------|
| `ceph.dir.rbytes` | Total bytes in the subtree (logical file sizes) |
| `ceph.dir.rfiles` | Total files in the subtree |
| `ceph.dir.rsubdirs` | Total subdirectories in the subtree |
| `ceph.dir.rctime` | Newest change time (ctime) anywhere in the subtree |
| `ceph.quota.max_bytes`, `ceph.quota.max_files` | Quota limits, if one is set on this directory |

**What stomp uses them for:**

- Instant usage for each grant directory, root and excluded directory (§8).
- Incremental scans: a subtree that hasn't changed since the last scan is skipped (§9.2).
- Finding archive candidates without a scan: `rbytes / rfiles` is the average file size, and directories full of small, old files are the best candidates.
- Checking quota headroom before archiving (§12.4).
- A live `du` and the TUI (§17, §19).

**Caveats:**

- **The totals are updated lazily**, so they can lag behind recent writes. Reports print the measurement time. **Nothing destructive ever relies on these totals**; deleting originals requires checksum verification (§12.4).
- **`rbytes` counts logical size.** Sparse files count at full size, and the extra raw space Ceph uses to store copies of the data is not included. That is the right number for attributing usage to grants.
- **The lag can make an incremental scan miss a change.** If a directory's change time is updated only after a scan has started, the next scan could skip a subtree that did change. Two measures prevent this: a safety margin on comparisons, and periodic full rescans (§9.2).
- **CephFS snapshots live in `.snap` directories.** stomp never enters them, and `rbytes` does not include snapshot data.

### 6.2 POSIX backend

This backend is used for roots that are not on CephFS, and on other clusters. It has no instant usage, so usage numbers come from the most recent scan. Scans cannot skip unchanged subtrees, so every scan is a full walk.

## 7. Configuration

### 7.1 Files and locations

stomp follows the XDG conventions (`$XDG_CONFIG_HOME` and `$XDG_DATA_HOME` override the defaults):

```
~/.config/stomp/config.toml        configuration (grants, roots, destinations, policies, Slurm)
~/.local/share/stomp/              catalog, inbox, task files, manifests (§10)
```

### 7.2 Example

```toml
[general]
roots = ["~"]                        # add other directories as needed
# staging_dir = "/scratch/$USER/stomp" # where chunks are built before upload; default: inside the archived directory
# rate_per_tb_month  = 0.0           # optional rates; enable cost estimates in reports
# rate_per_cpu_hour  = 0.0
# rate_per_gpu_hour  = 0.0

[slurm]
account   = "myaccount"
partition = "standard"
time      = "12:00:00"
cpus      = 8
mem       = "16G"
auto_resubmit = true                 # continue long tasks after a wall-time limit

[scan]
max_threads   = 8                    # cap on parallel file lookups; protects the shared metadata server
full_every    = "30d"                # periodic full rescan (Ceph); see §9.2
rctime_margin = "1h"
exclude = [                          # user exclusions (glob patterns)
  "**/node_modules",
  "**/__pycache__",
]
# environment and cache detection is built in; see §9.4

[archive]
chunk_size = "50GiB"
zstd_level = 3
only_owned = true                    # never archive or delete files owned by other users
default_destination = "nas"          # used when `stomp archive --to` is not given; "local" is also valid

[compute]
infer_from_workdir = true            # attribute jobs by working directory (§15.2)
history_start = 2025-01-01           # how far back to import accounting records on first sync

[[destination]]
name   = "nas"
type   = "sftp"
host   = "nas"                       # a Host alias from ~/.ssh/config works
path   = "/archive/$USER"
parallel_uploads = 4
verify = "remote-sha256"             # "remote-sha256" | "readback" | "size"; see Q1

[[destination]]
name     = "spaces"
type     = "s3"
endpoint = "https://nyc3.digitaloceanspaces.com"   # DigitalOcean Spaces; omit for AWS
region   = "us-east-1"                 # Spaces ignores it, but the SDK requires one
bucket   = "my-lab-archive"
prefix   = "stomp/$USER"
profile  = "spaces"                    # profile in ~/.aws/credentials holding the Spaces key
# force_path_style = true              # needed by some other S3-compatible services
# storage_class = "STANDARD"           # AWS only; cold classes are deferred (§13.3.5)
parallel_uploads = 4
verify = "server-checksum"             # "server-checksum" | "readback" | "size"

[[grant]]
id    = "GRANT-A"
title = "Example grant A"
pi    = "Dr. Example"
start = 2024-07-01
end   = 2027-06-30
dirs  = ["~/project_alpha"]

[[grant]]
id    = "GRANT-B"
title = "Example grant B"
end   = 2025-12-31
dirs  = ["~/project_beta", "~/project_beta_runs"]   # one grant, two directories

[[policy]]
name        = "archive-ended-grants"
match       = { grant_ended_for = "90d" }
action      = "propose"              # "propose" (default) | "apply"
destination = "spaces"               # a destination name, or "local" [default: archive.default_destination]

[[policy]]
name        = "pack-stale-small-files"
match       = { unchanged_for = "180d", min_files = 10000, max_avg_file_size = "1MiB" }
action      = "apply"                # archived automatically by the daily run (§14.4)...
wait        = "14d"                  # ...this long after first being proposed [default: 7d]
destination = "local"
```

### 7.3 Validation rules

- Expand `~` and `$USER`, then canonicalize all paths.
- Roots may not be nested inside other roots.
- Every grant directory must be inside a root.
- A directory claimed by two grants is an **error**. So is one grant's directory nested inside another grant's directory.
- One grant directory nested inside another directory of the same grant is a **warning**, because it is redundant.
- A grant directory that doesn't exist is a **warning**: it may have been archived or renamed.
- Destination names must be unique, and `local` is reserved. Every destination named in `default_destination` or in a policy must exist.

### 7.4 `stomp init`

`stomp init` gets a new user started:

1. Proposes roots (default `~`) and detects the backend for each one.
2. Lists the top-level directories of each root by size and file count. On Ceph this is instant.
3. Flags excluded directories it detects (environments, caches).
4. Writes a `config.toml` skeleton with one commented-out `[[grant]]` stub per large top-level directory, ready for the user to fill in.
5. If `~/.ssh/config` or `~/.aws/config` exists, adds commented-out `[[destination]]` stubs.
6. Never overwrites an existing config.

## 8. Storage attribution

### 8.1 Algorithm (Ceph)

For each root `R`:

```
total(R)       = rbytes(R)
usage(d)       = rbytes(d)                for each grant directory d in R
grant(g)       = Σ usage(d)               over g.dirs
unassigned(R)  = total(R) − Σ usage(d)    over the grant directories in R
```

The same calculation is done for file counts (`rfiles`).

The validation rules in §7.3 ensure grant directories never overlap, so this subtraction is exact. Excluded directories found by the last scan are measured with `rbytes` too. Reports show them as "of which" lines inside their grant (or inside unassigned), not as separate grants.

On a POSIX backend the same calculation uses totals from the most recent scan.

### 8.2 Samples and history

- Every `stomp usage` run records a usage sample for each root and grant directory. The scheduled task (§14.4) also records one.
- **TB-months:** usage is added up over time between samples, in decimal terabytes (10¹² bytes) for an average month, which is how storage is usually billed; reports say so. If measurements are more than two days (`2 × schedule interval`) apart anywhere in the period, the numbers are marked *estimated*. `stomp report history` gives them in every format: JSON as `{measurements, tb_months}`, and `--tb-months` alone (CSV included) for spreadsheets.
- If `rate_per_tb_month` is set, reports also show an estimated cost.
- **Remapping a directory re-attributes its history.** Samples are stored per path and assigned to grants using the *current* config, so moving a directory to a different grant moves its whole history. The same applies to compute jobs attributed by working directory (§15.2). Mappings with effective dates are a possible later addition (§25).

### 8.3 Remote holdings

For each grant and destination, the catalog records archived bytes (original and compressed sizes). Reports show these as separate storage tiers.

### 8.4 Example

```
$ stomp usage
Root ~  (CephFS, measured 2026-11-16 14:52, quota 50.0 TiB, 61% used)

GRANT          DIRS   SIZE       FILES         EXCLUDED     LAST CHANGE
GRANT-A        1      12.4 TiB   38,112,004    1.1 TiB      2h ago
GRANT-B        2       8.9 TiB    2,004,551    —            3d ago
GRANT-C        1       3.2 TiB      410,220    220 GiB      41d ago
(unassigned)   —       6.1 TiB    9,880,112    2.4 TiB      5h ago
TOTAL                 30.6 TiB   50,406,887

Remote:  nas     GRANT-B 4.2 TiB (2.9 TiB compressed), 3 archives
         spaces  GRANT-C 1.1 TiB (0.7 TiB compressed), 1 archive
```

## 9. Scanning

### 9.1 Purpose

A scan collects what Ceph's totals can't provide:

- age and size profiles
- file types
- largest files
- archive candidates
- excluded-directory discovery
- directory-level change tracking

### 9.2 Behavior

- **Walk.** Each root is walked in parallel with at most `scan.max_threads` threads. When running in a task, this is also capped by `SLURM_CPUS_PER_TASK`.
- **Never follow symlinks, and never cross into another filesystem** (detected by a change in `st_dev`). Never enter `.snap`.
- **Excluded directories** are recorded with their size and not entered (§9.4).
- **Unreadable directories** are recorded with the error and skipped.
- **Incremental (Ceph only).** If a subtree's change time is older than (previous scan start − `rctime_margin`), it is skipped and its totals from the previous scan are reused.
- **Full rescan.** A full rescan runs every `scan.full_every`, or on request with `--full`. This catches anything the lag described in §6.1 caused an incremental scan to miss.
- **Resumable.** Directories two levels below each root are *work units*. Each unit is saved to the scan's result file as soon as it finishes.
  - stomp asks Slurm to signal the task shortly before its wall-time limit (`--signal=B:USR1@300`). On that signal, or SIGTERM or Ctrl-C, the scan stops starting new work and exits marked *incomplete*.
  - `stomp scan --resume`, or an automatic resubmit, continues from the result file. Finished units aren't walked again; only the few directories above the units are re-read.
  - The result file is written without waiting for the disk to sync each commit (SQLite `synchronous = OFF`, with the journal kept in place). Syncing every commit made scans about 20 times slower, and on CephFS every journal file created and deleted is a metadata-server round trip. A killed process loses nothing this way; an OS crash could lose the last few units, which resuming simply redoes (D19).

### 9.3 What is recorded

The catalog stores totals **per directory, not per file**. Tens of millions of per-file rows would use gigabytes of the user's own quota for every scan.

For each recorded directory:

- path, and whether it is a root, a grant directory or an excluded directory
- bytes and files, both directly inside the directory and in its whole subtree
- newest and oldest mtime, and the newest change time
- a histogram of files and bytes per **modification month**
- a size breakdown of files and bytes: `<4K`, `4K–1M`, `1M–100M`, `100M–10G`, `>10G`

**Why months rather than age bands.** Reports show age bands (`<30d`, `30–90d`, `90d–1y`, `1–3y`, `>3y`), computed from the month histogram at report time and accurate to about two weeks. Storing the bands themselves would go stale: an incremental scan reuses an unchanged subtree's numbers, but that subtree's files keep getting older. Months are absolute, so reused numbers stay correct (D18).

**Grants aren't stored on rows.** Grant attribution comes from the current config at report time, as for usage samples (§8.2).

For each grant and each root, recorded only by full scans that ran without interruption:

- file extensions, by bytes and file count
- the 100 largest files

Incremental scans don't walk unchanged subtrees, and a resumed scan has lost the statistics gathered before the interruption, so neither could produce complete lists. Reports use the newest scan that has them, and say which scan that was.

**Small directories are folded into their parent.** A directory under 1 MiB that also has fewer than 100 files is not stored separately; its numbers are added to its parent's (both thresholds are configurable). This bounds catalog size on trees with many tiny directories.

**Optional per-file index.** Setting `track_files = true` on a grant directory, or on any directory, records every file in it. This allows exact file-level change lists for small, important trees.

`atime` (last access time) is recorded only as a hint, because it may not be updated reliably on CephFS. All age rules use `mtime` and the change time.

### 9.4 Excluded directories

| Kind | Detected by |
|------|-------------|
| environment | a `.singularity.d/` entry (Apptainer/Singularity sandbox), `conda-meta/` (conda env) or `pyvenv.cfg` (venv) |
| cache | known paths: `~/.cache`, `~/.apptainer/cache`, `~/.singularity/cache`, `~/.conda/pkgs` |
| user | glob patterns in `scan.exclude` |
| stomp | stomp's own data directory and staging directory |

Excluded directories are:

- **counted** in usage, at full size,
- **not entered** by detailed scans,
- **never archived**. If an archive target contains one, it is left in place and listed in the manifest's skipped section.

Caches are shown as *reclaimable*, along with the command that cleans them (for example `apptainer cache clean` or `conda clean -a`).

## 10. Catalog and state

### 10.1 Layout

```
~/.local/share/stomp/
  catalog.db                 SQLite; written only by the foreground CLI
  catalog.lock               lock file for ingestion
  inbox/                     task results waiting to be ingested
  inbox/jobs/                compute-job log records (§15.1)
  tasks/<task-id>/           generated sbatch script, logs, progress journal, checkpoint
  manifests/<archive-id>/    local copies of archive manifests
```

stomp creates all of these with mode `0700` or `0600`.

### 10.2 Only one process writes the catalog

- **SQLite is built into the binary** and uses rollback-journal mode, not WAL. WAL needs shared memory, which isn't reliable on a network filesystem.
- **Neither tasks nor `stomp jobs log` ever write `catalog.db`.** They write self-contained files to `inbox/`. Tasks also keep a progress journal in `tasks/<id>/`.
- **The scheduled daily run (§14.4) is the exception:** it opens the catalog like a command typed at the prompt, for the seconds its imports take (D33). Its heavy work, the weekly scan, is a separate task that follows the rule above.
- **The next CLI command ingests the inbox** before doing anything else, holding an `flock` on `catalog.lock` while it does. `stomp ingest` does the same thing explicitly.
- **`stomp tasks` shows live progress** by reading the task journals without changing them.

### 10.3 Tables and retention

Main tables:

- `grants`: a snapshot of the config at each ingest
- `usage_samples`
- `scans` and `dir_stats`
- `excluded_dirs`
- `destinations`
- `archives`, `chunks` and `archive_events`
- `proposals`
- `tasks`
- `compute_jobs`: accounting records copied from `sacct`
- `job_tags`: explicit grant assignments

Per-file manifests are stored as files, not as rows. The catalog indexes only chunks and path prefixes.

Retention, configurable:

| Data | Kept |
|------|------|
| Per-directory detail | the last 4 scans, plus one scan per month for 12 months |
| Per-grant and per-root summaries | forever |
| Usage samples | forever (they are tiny) |
| Compute job records and tags | forever (about one row per job) |

**Target:** the catalog stays under a few hundred MB for a user with tens of millions of files. `stomp doctor` reports the catalog's size.

## 11. Change tracking

There are three levels:

1. **Per grant** (the default): `stomp changes [--since AGE|DATE] [--grant G]`
   - Takes a fresh measurement (instant on CephFS) and compares it with a recorded one: the newest, or the newest at or before `--since`.
   - Shows each grant's size then and now, the change, the change in file count, and when anything last changed.
   - This doesn't record the new measurement, so repeated runs keep comparing with the same baseline.
2. **Directory-level:** `stomp changes --dirs [--since AGE|DATE] [--grant G] [--depth N] [--top N]`
   - Compares the newest scan with the previous one, or with the newest scan at or before `--since`.
   - Lists the largest growth and shrinkage under each root, or under the grant's directories, down to `--depth`, and marks directories as new or gone.
   - Small directories are folded into their parents (§9.3), so their changes show up in the parent.
3. **File-level** (deferred): added, removed and modified files, only in directories with `track_files = true`.

stomp keeps a log of its own actions (archive, restore, deletion of originals). A drop in usage caused by archiving is labeled as such instead of looking like data loss.

## 12. Archiving

### 12.1 Modes

- **local:** compress in place. After archiving, directory `D` contains `D/STOMP_ARCHIVED.txt` and `D/.stomp-archive/<archive-id>/` holding the chunks, manifest, `SHA256SUMS` and `README.txt`. The usage stays attributed to `D`'s grant.
- **remote** (`--to <destination>`): the chunks are uploaded to a destination (§13). `D` keeps only `STOMP_ARCHIVED.txt`, plus any files that were skipped.
- **move:** `stomp archives move <id> --to <destination>` sends an existing local archive to a destination without compressing it again. It can also move an archive from one destination to another (for example NAS → S3). Data passes through the HPC: download, verify, upload, verify, then delete from the old location.

The `STOMP_ARCHIVED.txt` note explains in plain language what happened: when, by whom, where the data went, and how to restore it with or without stomp. Collaborators browsing the directory are never left guessing.

### 12.2 Format

- **Chunks:** `chunk-0001.tar.zst`, …
  - POSIX tar (PAX format, for long names and precise timestamps), compressed with zstd (default level 3, multithreaded).
  - Each chunk is self-contained and can be extracted on its own.
  - Target size is `archive.chunk_size`. A file is never split across chunks; a file larger than the target goes into a chunk by itself.
  - Files are packed in sorted path order, so a subdirectory lands in as few chunks as possible and partial restores fetch only a few chunks.
- **`manifest.jsonl.zst`:**
  - A header record holding the archive ID, source path, grant, user, host, stomp version, creation time and totals.
  - One record per entry holding the relative path (base64 if not valid UTF-8), type, size, mode, mtime, uid, gid, chunk number and, for files, a BLAKE3 hash.
  - A *skipped* section listing what was left in place and why.
  - Readable with `zstdcat manifest.jsonl.zst | jq`.
- **`SHA256SUMS`:** checksums of every chunk and the manifest, checkable with `sha256sum -c`.
- **`README.txt`:** instructions for restoring without stomp:

  ```
  sha256sum -c SHA256SUMS && for c in chunk-*.tar.zst; do zstd -dc "$c" | tar -x; done
  ```

**Archive IDs** look like `<grant>-<yyyymmdd>-<6 hex characters>`, for example `GRANT-B-20261116-a3f9c2`.

The layout is the same everywhere: in `.stomp-archive/` for local archives, and at every destination.

### 12.3 What is included

- **Archived:** regular files, directories, symlinks (stored as links and never followed) and hard links (preserved within the archive).
- **Left in place and listed in the manifest's skipped section:**
  - excluded directories (§9.4)
  - files owned by other users (when `only_owned = true`, the default)
  - sockets, FIFOs and device files
  - unreadable files
- **`stomp archive --estimate`** compresses a sample of the files and predicts the compressed size, the reduction in file count and how long the task will take, before anything is committed. zstd stores data that won't compress without spending much CPU, so stomp doesn't sort files by type.

### 12.4 Procedure and safety invariants

Each chunk goes through these steps, and every step is written to a journal:

```
PLAN → WRITE → VERIFY_LOCAL → [remote: UPLOAD → VERIFY_REMOTE → DELETE_LOCAL_CHUNK] → DELETE_SOURCES → DONE
```

- **WRITE** builds the chunk with tar and zstd. While writing, it hashes each file's bytes as they are read (BLAKE3) and hashes the chunk as it is written (SHA-256).
- **VERIFY_LOCAL** reads the chunk back, decompresses it, recomputes every file's BLAKE3 hash and compares them.
- **UPLOAD and VERIFY_REMOTE** depend on the destination type (§13).
- **A remote archive is complete only once its manifest exists.** The manifest, `SHA256SUMS` and `README.txt` are uploaded last, after every chunk has been verified. An archive directory without a manifest is incomplete, so a crash never leaves something that looks finished but isn't.

**Invariants:**

| ID | Invariant |
|----|-----------|
| I1 | No source file is deleted until the chunk that holds it has passed verification at its final destination. |
| I2 | Just before deleting a source file, stomp checks it again. If its size, mtime or ctime has changed since it was archived, the file is kept and flagged *changed during archive* in the manifest and the report. |
| I3 | A directory is removed only when it is empty after its files have been deleted. stomp never deletes a whole tree recursively. |
| I4 | Every destructive step is recorded in the journal before and after it runs. A killed task resumes from the journal and never deletes or uploads twice. |
| I5 | Destructive commands show a plan and ask for confirmation (`--yes` for non-interactive use). `--dry-run` works on every command. |
| I6 | stomp modifies nothing outside the archive target, its data directory, its staging directory and its paths at the destinations. |

**Space headroom.** Each chunk is finished before the next begins, so the extra space needed is about `chunk_size × chunks in progress`, not the size of the whole directory. Before starting, stomp compares this with the quota headroom (`ceph.quota.*`, if set) and refuses to start if there isn't enough room. Chunks can be built in a `staging_dir` such as scratch.

### 12.5 Restore

`stomp restore <archive-id | path> [--only <glob>] [--to <dir>] [--overwrite] [--delete-remote]`

- The manifest identifies which chunks are needed. Only those chunks are downloaded and extracted.
- Each chunk's SHA-256 is checked after download, and each file's BLAKE3 hash after extraction.
- **By default, data is restored to its original location.** Existing files are never overwritten without `--overwrite`. `--to` restores somewhere else.
- File mode and mtime are restored. The restored files are owned by the user running the restore.
- After a complete, verified restore:
  - local archive chunks are removed, since the original data is back,
  - the remote copy is kept unless `--delete-remote` is given.
- If the chunks are in a cold S3 storage class, the restore runs in two phases (§13.3.5).

### 12.6 As built (M3: local archives)

#### What stomp refuses to archive

`stomp archive` refuses:
- a root itself
- a directory containing another grant's directories (archive each grant's directories separately)
- anything inside a `.stomp-archive`
- an environment, cache or excluded directory
- a directory containing stomp's own data
- a directory that already has a `STOMP_ARCHIVED.txt`, unless resuming

#### Planning

A parallel walk lists the entries, sorted by name within each directory, and assigns them to chunks.
- **The plan is saved in the archive directory and fixed for the rest of the archive.** Files created later aren't in it, so they stay where they are (D21).
- **Planning holds the whole list in memory, about 150 bytes per file.** That's roughly 1.5 GB per 10 million files. Size `--mem` for archive tasks accordingly.

#### The note

The note is written as soon as planning finishes ("being archived…", with how to resume or restore), so a half-finished directory always explains itself. It is rewritten when the archive finishes.

#### For each chunk

1. **WRITE:** written to `chunk-NNNN.tar.zst.partial`, synced, then renamed into place. Per-file records (BLAKE3 hash, inode, size, mtime and ctime to the nanosecond) go to `manifest.d/chunk-NNNN.jsonl`.
   - A file is opened without following symlinks.
   - A file that shrank, grew or changed while being read is still recorded, but marked `changed`. It is never deleted.
2. **VERIFY:** the chunk's SHA-256 is checked, then it is fully decompressed and every file's BLAKE3 hash compared (D23).
   - On a mismatch, the chunk is deleted and a `discarded` event is journaled. Nothing from it is deleted, and the next run writes it again.
3. **DELETE:** first every original in the chunk is checked, then the unchanged ones are removed (D22).
   - **Regular files and symlinks** must match their recorded inode, size, mtime and ctime.
   - **Hard links** are checked by inode, size and mtime. Removing one link changes the shared inode's ctime, so ctime can't be used.
   - **Kept files:** originals that changed or couldn't be removed are listed in `chunk-NNNN.kept.jsonl` and marked `kept` in the final manifest.

#### Finishing

After all chunks, directories that are now empty are removed, deepest first. stomp then writes the final manifest (header, entries, then what was left in place), `SHA256SUMS`, `README.txt` and the final note, and removes its working files (`plan/`, `manifest.d/`).

#### Resuming

The journal is replayed on every run, and an interrupted step is redone:
- a half-written chunk is rewritten
- an unverified chunk is verified
- an interrupted deletion is repeated; it's idempotent, since it only removes originals that still match their record

Tests stop the run after every journaled step in turn, resume it, restore, and compare every file, mode, mtime, symlink and hard link with the original.

#### Restore

- **Resumable:** a file whose size and mtime already match counts as restored (D24).
- **Kept originals are left alone:** files the archive marked `kept` are newer than the archived copies.
- **When the archive is removed:** only after a complete restore of everything into the original location, with no conflicts.
- **Restoring all of an unfinished archive undoes it (D38).** What was archived so far comes back and the rest never left, so the archive's files (and any chunks already uploaded, and unfinished S3 uploads) are removed, the note goes, and the catalog records the archive as `abandoned`. The directory can then be archived afresh. stomp refuses while the archive's task is still queued or running.
- **`--dry-run`** prints the plan: files, chunks (and how many must be downloaded), what's already in place, conflicts, and what happens to the archive afterwards. Before submitting a task, stomp checks the `--only` patterns against the local manifest, so a pattern that matches nothing fails at once; `--to` is created only by the restore itself.

#### Other commands

- `stomp archives list` lists archives from the catalog, with a WHERE column: `directory`, a destination's name, or `—` once nothing remains. An unfinished archive shows its planned size and start time (the catalog hears of it once the plan is journaled).
- `stomp archives show <ID|DIR>` summarizes one archive, starting with where its records and its chunks are.
- The confirmation prompt, the estimate's file count and the note all reflect the destination: a remote archive leaves the six record files, and the note names where the chunks went on a `Chunks:` line.
- `stomp archives verify <ID|DIR> [--deep]` checks the checksums; `--deep` also decompresses every chunk and checks every file.

## 13. Remote destinations

### 13.1 Common interface

Every destination type implements the same operations:

- `put`: resumable, and atomic as far as the destination allows
- `verify`
- `get`: the whole object or a byte range
- `list`
- `delete`
- `usage`: the space the user's stomp archives take up there

**Layout** is identical across destination types:

```
<base>/<grant-id>/<archive-id>/
    README.txt
    manifest.jsonl.zst
    SHA256SUMS
    chunk-0001.tar.zst
    ...
```

`<base>` is `path` for SFTP and `s3://<bucket>/<prefix>` for S3.

**Re-checking.** `stomp archives verify <id> --remote` checks a remote archive again at any time.

### 13.2 SFTP

- **Transport.** stomp runs the system's OpenSSH `ssh` and `sftp` programs as subprocesses rather than embedding an SSH library. This reuses the user's working `~/.ssh/config` (host aliases, ProxyJump, keys, ssh-agent, known_hosts) and keeps cryptography out of the static binary. stomp always passes `-o BatchMode=yes`, so a task fails quickly instead of hanging at a password prompt.
- **Uploads.** Each file is uploaded to `*.partial` and renamed only after it passes verification. Interrupted uploads resume with OpenSSH's `reput`.
- **Throughput.** stomp runs `parallel_uploads` sftp sessions at once; a single SFTP stream is slow.
- **Verification** (`verify`), which depends on Q1:
  - `remote-sha256`: run `sha256sum` on the server over `ssh`. This needs shell access to the server.
  - `readback`: download the file again over SFTP and hash it locally. This works on SFTP-only servers but doubles the transfer.
  - `size`: compare file sizes only. This is weak, so it must be chosen explicitly.

  `stomp doctor` detects which of these works.

### 13.3 S3

#### 13.3.1 Client

- **Library.** stomp uses the AWS SDK for Rust (`aws-sdk-s3`), compiled in behind a cargo feature `s3` that is on by default.
- **Credentials and settings.** The SDK reads the user's existing `~/.aws/config` and `~/.aws/credentials` files, profiles and `AWS_*` environment variables natively. This is the same reuse-what-already-works reasoning as SFTP (D6), and no extra programs are needed.
- **S3-compatible services** (DigitalOcean Spaces, campus Ceph object storage, MinIO, Wasabi and others) are supported with `endpoint` and, where needed, `force_path_style`.

#### 13.3.2 Uploads

- **Chunks are uploaded in parts** (multipart upload), with parts uploaded in parallel up to `parallel_uploads`.
  - Part size is the larger of 64 MiB and `chunk_size / 9000`, which stays under S3's limit of 10,000 parts.
  - The upload ID is recorded in the journal. An interrupted upload resumes by listing the parts already uploaded.
- **Atomic.** An S3 object becomes visible only when its multipart upload is completed, so no `.partial` naming is needed.
- **Cleanup.** stomp aborts the incomplete uploads it started when an archive is cancelled. The docs recommend a bucket lifecycle rule that aborts incomplete multipart uploads after a few days, as a backstop.
- **Metadata.** Each chunk is stored with `x-amz-meta-stomp-sha256` (the whole chunk's SHA-256) and `x-amz-meta-stomp-archive` (the archive ID).

#### 13.3.3 Verification (`verify`)

- **`server-checksum`** (default): every part is sent with a checksum that the server checks on arrival, rejecting any mismatch. stomp uses the strongest kind the server enforces:
  1. a SHA-256 checksum (`x-amz-checksum-sha256`), or
  2. `Content-MD5`, which almost every S3-compatible service enforces.

  Some services silently ignore checksum headers. `stomp doctor` finds out which kind the server actually enforces: it uploads a small object with a deliberately wrong checksum of each kind and confirms the upload is rejected. If the server enforces neither, stomp refuses to use `server-checksum` for that destination.
- **`readback`:** download the object again and hash it locally. This costs download traffic, which AWS charges for.
- **`size`:** compare sizes only. This is weak, so it must be chosen explicitly.

#### 13.3.4 Restores

- Only the needed chunks are fetched, as with SFTP.
- Downloads use parallel byte-range requests for throughput.

#### 13.3.5 Cold storage classes (deferred; AWS only)

Spaces has no cold storage classes, so this section applies only to an AWS destination. It is deferred to M6.

Objects in GLACIER, DEEP_ARCHIVE and similar classes must be *thawed* before they can be downloaded, which takes minutes to 48 hours. With a cold class:

- **Small files stay readable.** The manifest, `SHA256SUMS` and `README.txt` are always stored in STANDARD, so listing and restore planning never need a thaw.
- **Restores run in two phases.** `stomp restore` sends thaw requests for the needed chunks and records the restore as *thawing*. Later, `stomp restore --resume <id>` (or the scheduled task) downloads the chunks once they are available.
- **`stomp archive --estimate` shows costs.** It lists storage, request and retrieval costs, including minimum storage durations: 90 days for Glacier Flexible Retrieval, 180 days for Deep Archive. Larger chunks mean fewer requests.

#### 13.3.6 Credentials in tasks

Long-lived access keys or an assumed-role profile work in tasks. Short-lived credentials, such as SSO sessions, can expire partway through a long task. `stomp doctor` and task submission both warn when the credentials will expire before the task's time limit.

#### 13.3.7 DigitalOcean Spaces

Spaces is the first S3 service stomp targets, because the user already keeps data there.

- **Endpoint:** `https://<region>.digitaloceanspaces.com`, for example `nyc3`. The SDK's `region` is set to `us-east-1`; Spaces ignores it, but the SDK requires a value.
- **Credentials:** a Spaces access key, stored as a profile in `~/.aws/credentials`. These keys don't expire, so they work in long tasks.
- **SDK checksum defaults:** recent AWS SDKs add CRC checksums to uploads and downloads by default, and some S3-compatible services reject them. For endpoints that aren't AWS, stomp sets the SDK to send checksums only when required, and adds the checksum kind that `doctor` selected (§13.3.3) itself.
- **Rate limits:** Spaces limits request rates. stomp keeps `parallel_uploads` modest and retries with backoff when the server answers `503 SlowDown`.
- **Costs:** Spaces bills outbound transfer beyond the plan's allowance, so `readback` verification costs money. `server-checksum` is preferred.
- **No storage classes:** `storage_class` is ignored, and cold storage (§13.3.5) isn't available.
- **Reachability:** Spaces is on the public internet, so compute nodes may not be able to reach it (§13.4). `stomp doctor --submit` will tell.

### 13.4 Reachability from compute nodes

A destination that works from a login node may not work from a compute node. For example, AWS endpoints need outbound internet access, which many clusters block on compute nodes.

- `stomp doctor --submit` runs the checks, destinations included, in a short task on a compute node.
- If a destination can't be reached from compute nodes, the work splits in two:
  1. Archive locally in a task (`stomp archive DIR --to local`).
  2. From a login or data-transfer node, run `stomp archives move <ID> --to <destination> --here`. This step mostly waits on the network rather than the CPU.

### 13.5 As built (M4)

#### The destination interface

Every destination provides:
- `put`: upload and verify; a file exists at the destination only once it is complete and correct
- `verify`
- `get`
- `delete` and `delete_dir`
- `list`
- `check`, the self-test that `stomp doctor` runs

Every destination uses the same layout: `<base>/<grant or "unassigned">/<archive id>/`.

#### Remote archives

For each chunk:

```
WRITE → VERIFY (fully decompressed) → UPLOAD (put, verified at the destination)
      → remove the local chunk → DELETE originals
```

Then:

```
Finalized → upload README, SHA256SUMS, then the manifest last → Finished
```

- **Where chunks are written:** `general.staging_dir/<id>` if it's set, otherwise the archive directory.
- **What stays local:** the archive's records (manifest, SHA256SUMS, README, settings, journal) stay in `<dir>/.stomp-archive/<id>`. Listing, planning restores, and `show` never need the network (D28).
- **The note and README** say where the chunks are, and how to fetch them and restore without stomp.

#### S3 (D25, D26)

- **The client:** SigV4-signed requests over HTTPS (ureq with rustls). Signing is tested against AWS's published examples.
- **Credentials:** read from `AWS_*` environment variables when no profile is given, otherwise from the named profile in `~/.aws/credentials` or `~/.aws/config`.
- **Retries:** network errors, `429` and `5xx` (including `503 SlowDown`) are retried with backoff.
- **Small and large uploads:**
  - A chunk no larger than the part size goes up with a single `PUT`.
  - Larger chunks use a multipart upload: parts of max(64 MiB, size/9000), uploaded in parallel (`parallel_uploads`).
  - The upload ID is kept in a `.upload` file beside the local chunk. An interrupted upload asks the server which parts it already has, and resends only the missing or mismatched ones.
- **Integrity checks:**
  - **Every request body is checked by the server twice.** Its SHA-256 is part of the signature, and a Content-MD5 header is sent too.
  - **The object records its own checksum.** It carries `x-amz-meta-stomp-sha256`.
  - **What each `verify` mode checks after the upload:**
    - `server-checksum` (default): the object's size and that recorded checksum.
    - `readback`: downloads the object and hashes it.
    - `size`: the size alone.
- **What `stomp doctor` checks:** it uploads a test object, then sends deliberately wrong checksums. If the server accepts both while `verify = "server-checksum"`, that's a failure, with advice to use `readback`.
- **Avoiding SDK checksum problems:** stomp never sends `x-amz-checksum-*` headers or chunked (`aws-chunked`) uploads, so the SDK checksum problems some S3-compatible services have (§13.3.7) don't arise.

#### SFTP (D27)

- **Batch mode:** stomp runs `sftp -b -` and `ssh`, both with `BatchMode=yes`.
- **Uploads:** each file goes to `<file>.partial`, is verified, then renamed into place.
- **Verification:** `verify` defaults to automatic: `sha256sum` over SSH, and if the server refuses commands, reading the file back. Setting `remote-sha256` makes a refusal an error; `size` checks the size alone.
- **Interrupted uploads start over.** A `.partial` file is always rewritten, which is simple and safe.
- **One stream:** each upload uses a single SFTP stream; `parallel_uploads` applies to S3 only.
- **Testing:** commands and output parsing are tested against stand-in `sftp`/`ssh` programs. Behavior against a real OpenSSH server, such as quoting edge cases, is still to be confirmed on the NAS.

#### Restore

- Chunks that aren't on disk are downloaded into the staging location, verified, extracted, and deleted.
- After a complete restore, the remote copy is kept and the archive is recorded as `restored-copy-kept`, unless `--delete-remote` is given.
- **The kept copy stays usable (D37).** Its records (settings, journal, `SHA256SUMS`, `README.txt`, and the manifest if it's 64 MiB or less) move to `~/.local/share/stomp/manifests/<ID>/`, and the directory becomes ordinary again. `stomp archives show`, `verify` and `restore` find them by ID or by the directory; a manifest that wasn't kept is downloaded and checked for the length of one command. Restoring from them downloads chunks next to the files being restored, not into the data directory.
- **`stomp archives delete <ID>`** deletes a kept copy, after asking: everything under its prefix (manifest first, so an interrupted deletion never looks complete), unfinished S3 multipart uploads, and the kept records. It refuses an archive that still holds the only copy of its files.
- **Deleting at a destination** (moves, `--delete-remote`, undoing, `archives delete`) always removes the manifest first and aborts unfinished multipart uploads (D39).

#### Moving archives

- `stomp archives move <ID> --to <destination|local>` moves an archive between its directory and a destination, or between two destinations.
- Each chunk is copied and verified at the new place, and journaled as `Moved`, before any copy of it is removed.
- The metadata goes next. The old location is cleared last, and only then are the saved settings updated. Interrupted moves resume.

#### Reports

- `stomp usage` lists what's archived elsewhere, by destination and grant.
- `stomp report grant` lists the grant's archives.
- `stomp report lifecycle` shows how much each grant has archived elsewhere.

## 14. Slurm integration (stomp's own tasks)

### 14.1 Where commands run

- **Heavy commands** are `scan`, `archive`, `restore`, `archives verify --remote` and `archives move`.
  - Run outside a Slurm job, they submit a **task** by default and print its ID.
  - `--here` runs them in the foreground instead, with a warning if on a login node.
  - Run inside a Slurm job (`SLURM_JOB_ID` is set), they run directly.
- **Cheap commands** (`usage`, `du`, `report`, `changes`, `jobs`, `tasks` and the config commands) always run in the foreground.

### 14.2 Task generation

- Task settings come from `[slurm]`, and each command can override them (`--time`, `--cpus`, `--mem`, `--partition`, `--account`).
- The generated script is saved to `tasks/<id>/task.sh` so the user can inspect it. Logs are kept next to it.
- Tasks request `--signal=B:USR1@300`. On that signal the task saves a checkpoint and exits cleanly. With `auto_resubmit` set, stomp resubmits the task to continue, up to a limit.
- **Tasks attribute themselves (§15).** An archive or restore task logs itself to the grant whose data it handles. Scans are logged as `stomp` overhead.

### 14.3 Task tracking

`stomp tasks` lists submitted tasks with their state (from `squeue` and `sacct`) and progress (from the task's journal). `stomp tasks cancel <id>` cancels one.

### 14.4 Recurring work

`stomp schedule enable [--at HH:MM] [--mode scrontab|task]` sets up **one small daily job** (1 CPU, 2 GB, 30 minutes, with `[slurm]`'s partition and account) (D31). Each run:

1. imports the inbox (finished scans, archive records, job logs);
2. records a usage sample, if every root reports usage instantly (CephFS); otherwise measuring would mean walking every file, so it says so and skips it;
3. imports job accounting (`stomp jobs sync`);
4. submits a scan task when the last scan started 6½ days ago or more, or resumes one that stopped early, unless one is already queued or running;
5. re-evaluates the policies against the newest detailed scan and estimates compression for new proposals (§16.1);
6. archives proposals from `apply` policies whose wait has passed, at most 3 a run, each as its own archive task (D32);
7. keeps the newest 30 run logs.

**How it's scheduled:**

- **`scrontab`**, if the site enables it (Slurm 20.11 or later). stomp adds a block between marker comments, with `#SCRON` option lines, and leaves the rest of the user's crontab alone.
- **Otherwise, a job that queues the next one.** It's submitted with `sbatch --begin` for the chosen time. Each run queues its successor first, so a run that fails still keeps the chain going.
- **The default time** is between 02:00 and 05:00, different for each user (from a hash of the user name), so colleagues' runs don't all start together.

**Managing it:**

- `stomp schedule status` shows the mode, the next queued run (in task mode), and the last run's outcome, line by line. It reports a problem if the scrontab entry or queued job is gone, or if no run has finished for over two days. `stomp doctor` reports the same problems.
- `stomp schedule disable` removes the entry or cancels the queued job.
- `stomp schedule run` does a run's work immediately.
- State is kept in `schedule/state.json` and logs in `schedule/logs/` under the data directory.

## 15. Compute job attribution

Goal: report each grant's compute use next to its storage use. The center doesn't attribute jobs to grants, so stomp does it on the user's side.

### 15.1 How a job gets its grant

These sources are used in order of precedence:

1. **Explicit log:** `stomp jobs log <GRANT>`, run inside the job script or afterwards with `--job`.
2. **Working directory** (`compute.infer_from_workdir`, on by default): if the job's working directory, as recorded by `sacct`, is inside a grant directory, the job belongs to that grant.
3. **Otherwise the job is unassigned.**

Reports show which source attributed each job. Moving a directory to a different grant re-attributes the jobs inferred from it (§8.2); explicit logs never change.

#### Logging from inside a job

```bash
#!/bin/bash
#SBATCH --job-name=run42
#SBATCH --time=04:00:00
stomp jobs log GRANT-A --note "calibration sweep"
srun ./model ...
```

- **What it records:** the job ID from `SLURM_JOB_ID`, plus the grant, a timestamp, the hostname and an optional `--note`.
- **Job arrays:** the record is keyed by `SLURM_ARRAY_JOB_ID`, so the whole array is logged once. Later tasks in the array see that the record exists and do nothing.
- **Fast and lock-free.** It creates one small file, `inbox/jobs/<job-id>.json`, by writing to a temporary name and renaming it. It doesn't open the catalog or take any locks, and it returns within milliseconds.
- **Never breaks the job.**
  - It always exits 0 and reports problems as warnings on stderr. `--strict` turns them into errors (said as `error:`, exit 1), and then an unknown grant isn't recorded.
  - The note is shown in `stomp jobs list`, as a NOTE column when any job has one.
  - An unknown grant ID is still recorded, with a warning. Reports show it as *unknown grant "X"* so a typo is visible and can be fixed with `stomp jobs retag`.
  - If the config can't be read, the record is still written.

#### Logging after the fact

| Command | Effect |
|---------|--------|
| `stomp jobs log GRANT-A --job 123456 [--job 123457 ...]` | Tag finished jobs |
| `stomp jobs log GRANT-A --name 'carbon_*' --since 2026-01-01` | Tag past jobs whose names match, found through `sacct` (deferred; working-directory inference already covers most history) |
| `stomp jobs retag <GRANT> <job-id>...` | Assign jobs to a grant by hand, overriding everything else |
| `stomp jobs untag <job-id>...` | Remove explicit tags, falling back to working-directory inference |

**Precedence when a job has several tags:** tags from `retag` (`manual`) and from stomp's tasks (`task`) always replace earlier tags. A tag logged from inside a job never replaces those two kinds, so a requeued job can't undo a manual fix.

### 15.2 Collecting accounting data

- **`stomp jobs sync`** imports the user's finished jobs from `sacct`. It runs automatically before compute reports and in the daily scheduled task. It asks for these fields, one line per job allocation, without job steps (`-X -P`):
  - `JobID`, `JobName`, `State`, `Submit`, `Start`, `End`
  - `ElapsedRaw`, `CPUTimeRAW`, `AllocTRES`
  - `WorkDir`, `Partition`, `Account`
- **Bounded queries.** Queries cover bounded time windows (one month at a time when filling in history), to keep load off the accounting database. The first sync goes back to `compute.history_start` (or 90 days). stomp remembers how far back imports reach, so setting `history_start` earlier later on fills in the older jobs at the next sync.
- **Records are copied into the catalog** (`compute_jobs`) because sites eventually purge old accounting records. Once copied, reports don't depend on `sacct` keeping them.
- **Running jobs** are shown as in progress and recorded when they finish.

### 15.3 Metrics

Per grant and per period:

| Metric | Derived from |
|--------|--------------|
| Jobs, by final state | `State` |
| CPU hours | `CPUTimeRAW / 3600` (allocated CPUs × elapsed time) |
| GPU hours | GPU count in `AllocTRES` (`gres/gpu=N`) × `ElapsedRaw / 3600` |
| Billing units, if the site records them | `billing=` in `AllocTRES` × elapsed time |
| Wall hours | `ElapsedRaw / 3600` |

- **Cost estimates** appear if `rate_per_cpu_hour` or `rate_per_gpu_hour` is set.
- **Failed and cancelled jobs** used allocation too, so they are counted, but reports also show them broken out by state.

### 15.4 Example

```
$ stomp report compute --from 2026-10-01 --to 2026-10-31
Compute by grant, 2026-10-01 to 2026-10-31  (sacct, synced 2026-11-16 15:02)

GRANT          JOBS   CPU-HOURS   GPU-HOURS   ATTRIBUTED BY
GRANT-A         412      18,340        612    log 380, workdir 32
GRANT-B          37       2,105          0    workdir 37
stomp             9          41          0    task 9
(unassigned)     58         940         12    —
TOTAL           516      21,426        624
```

## 16. Policies (automatic compression)

- **Rules.** Policies are written in the config (§7.2). The available conditions are:
  - `grant` (one ID or a list), `path` (glob)
  - `grant_ended_for`, `unchanged_for`
  - `min_bytes`, `min_files`, `max_avg_file_size`
- **Evaluation.** Policies are evaluated against the newest detailed scan, by `stomp candidates`, `stomp apply` and each scheduled run. The results are stored as **proposals**.
- **Reviewing proposals.** `stomp candidates` lists proposals with the estimated space freed and the change in file count. `stomp apply <id>... | --all` carries them out, and `stomp candidates dismiss <id>...` stops proposing a directory.
- **Defaults are safe.** The default action is `propose`, so nothing is archived automatically unless the user writes `action = "apply"` in a policy. When they do, the daily run (§14.4) submits archive tasks, and all the invariants in §12.4 still apply.

### 16.1 As built (M5)

**What a policy proposes.**

- **Every condition must hold.** A policy proposes the *outermost* recorded directories that meet all of its conditions. When policies overlap, only the outermost proposal is kept, since archiving a directory covers everything in it. For the same directory, the first policy in the config wins.
- **Grant data only (D29).** Without a `path`, only grant directories and directories inside them are proposed, and nothing under a hidden directory (a name starting with `.`).
  - **Why:** software, settings and reference data are often read every day without being modified, so age alone mustn't put `~/bin`, `~/.conda` or a shared genome index up for archiving.
  - **Other data:** a policy with a `path` glob can propose any directory the glob matches. `*` doesn't cross `/`, and `**` does.
- **Never proposed:**
  - roots
  - directories that contain another grant's directory
  - excluded directories (environments, caches, `scan.exclude`)
  - stomp's data and staging directories, and anything containing them
  - directories already archived or being archived, or that have gone since the scan
- **Sizes leave out excluded directories.** Environments and caches inside a proposal stay in place when it's archived, so they don't count toward its size or its conditions.
- **"Unchanged" uses both times (D30).** `unchanged_for` requires both the newest modification time (mtime) and the newest change time (ctime) to be older than the period.
  - Copying, `rsync -a` and restoring keep old mtimes but set new ctimes, so data that has just arrived isn't proposed straight away.
  - This is stricter than `stomp report stale`, which uses mtime alone.
- **Grant end dates.** `grant_ended_for` compares the grant's `end` date in the config with today.

**Proposals.**

- **They're kept in the catalog** (`proposals` table, schema v4) with a stable numeric ID.
- **Each evaluation updates the open ones.**
  - New matches are added.
  - Ones that no longer match are removed.
  - The rest keep their ID and when they were first proposed.
- **Applied and dismissed proposals are history.** They aren't proposed again, but `stomp apply <id>` still archives a dismissed one.
- **Space freed.**
  - For a remote destination, it's the whole size.
  - For a local one, it's the size times one minus the estimated compression ratio.
- **The estimate is cheap.** stomp compresses the first MiB of 16 files picked in proportion to size. The scan's per-directory sizes say where to look, so this reads little more than the samples.
  - It's made for new proposals: up to 8 per `stomp candidates` run, and all of them in the daily run.
  - It's made again when a proposal's size changes by more than 25%.
- **Files after archiving** are the chunks (for a local archive) plus the six record files.

**Applying.**

- **Each proposal is checked again first.** The policy must still be in the config, and the directory must pass the archive checks (§12.4). Its grant and path conditions must still hold under the current config.
- **On CephFS, nothing may have changed since the scan.** The live subtree totals and rctime must still meet the size and age conditions.
- **A directory that fails is skipped with the reason.**
- **Running it.** Each archive is its own Slurm task, or runs in the foreground one after another with `--here`.

**Automatic archiving (D32).**

- **An age condition is required.** A policy with `action = "apply"` must have `grant_ended_for` or `unchanged_for`.
- **It waits.** A proposal is archived only `wait` (default 7 days) after it was first proposed, giving the user time to see it in `stomp candidates` and dismiss it. `stomp usage` mentions waiting proposals.
- **It's capped.** The daily run starts at most 3 automatic archives, oldest proposals first, so a new policy doesn't flood the queue or the staging space.

## 17. Command-line interface

**Global flags:** `--config <path>`, `--json`, `--dry-run`, `-v`/`-q`.

| Command | Purpose | Runs |
|---------|---------|------|
| `stomp init` | Create a config from the detected layout | foreground |
| `stomp doctor [--submit] [--offline]` | Check the environment: backend, quota, Slurm, `sacct`, scrontab, catalog size, and each destination (login, writing, verification method, whether the server enforces checksums). `--submit` runs the checks from a compute node; `--offline` skips the destinations. | foreground / task |
| `stomp usage [--grant G] [--root R]` | Current storage use by grant (§8.4) | foreground |
| `stomp du [PATH] [--depth N]` | Directory tree with sizes; instant on Ceph | foreground |
| `stomp scan [--full] [--resume] [--here] [--time --cpus --mem --partition --account]` | Detailed scan; the Slurm options override `[slurm]` for this task | task |
| `stomp changes [--since T] [--dirs] [--grant G] [--depth N] [--top N]` | Storage change tracking (§11) | foreground |
| `stomp report <kind> [options] [--format table\|csv\|json\|md]` | Reports (§18) | foreground |
| `stomp find [--grant G] [--largest N] [--older-than T] [--ext E]` | Find files and directories, using scan data | foreground |
| `stomp candidates [--all]` / `stomp candidates dismiss <ID>...` | Review proposals, with estimated space freed and file counts (§16) | foreground |
| `stomp apply <ID>... \| --all [--yes] [--here]` | Archive proposed directories, each checked again first (§16.1) | task |
| `stomp archive <DIR> [--to local\|<dest>] [--dry-run] [--estimate] [--resume] [--level N] [--chunk-size S] [--yes] [--here]` | Archive a directory; `--dry-run` and `--estimate` only plan | task |
| `stomp archives list\|show\|verify [--deep]` | Manage existing archives; `verify` checks remote chunks where they're stored | foreground |
| `stomp archives move <ID> --to <DEST\|local> [--here] [--yes]` | Move an archive between its directory and destinations (§13.5) | task |
| `stomp restore <ID\|DIR> [--only PATTERN]... [--to DIR] [--overwrite] [--keep-archive] [--delete-remote] [--yes] [--here]` | Restore (§12.5); resumable by running it again | task |
| `stomp jobs log <GRANT> [--job ID]... [--name GLOB --since T] [--note TEXT] [--strict]` | Attribute compute jobs to a grant (§15.1) | instant |
| `stomp jobs list [--grant G \| --unassigned] [--since T] [--no-sync]` | Compute jobs with their grant and usage | foreground |
| `stomp jobs sync` | Import accounting records now; other commands do it when the last import is over 15 minutes old | foreground |
| `stomp jobs retag <GRANT> <JOB>...` / `untag <JOB>...` | Fix attributions (§15.1) | foreground |
| `stomp tasks [list\|show\|cancel]` | stomp's own background work (§14.3) | foreground |
| `stomp schedule enable [--at HH:MM] [--mode scrontab\|task]` / `disable` / `status` / `run` | The daily run (§14.4) | foreground / scheduled |
| `stomp ingest` | Import the inbox (normally automatic) | foreground |
| `stomp tui [DIR]` | Interactive browser (§19.1) | foreground |

**Exit codes:**

| Code | Meaning |
|------|---------|
| 0 | Success |
| 1 | Error |
| 2 | Invalid usage or config |
| 3 | Partial success; details in the output |

The one exception is `stomp jobs log`, which always exits 0 unless `--strict` is given (§15.1).

## 18. Reports

| Report | Contents |
|--------|----------|
| `usage` | Current per-grant storage, excluded directories and unassigned usage, quota, remote holdings by destination |
| `compute` | Per-grant jobs, CPU hours, GPU hours and billing units for a period, with how each job was attributed (§15.4) |
| `history` | Per-grant storage over time, with TB-months; optional cost estimates. Compute over time is `report compute --by-month`. |
| `grant <id>` | One grant's storage and compute together: directories, usage over time, TB-months, age profile, top extensions, largest files, archives by destination, CPU and GPU hours by month with a total and cost. History and compute cover `--from`/`--to`, by default the grant's start (or a year ago) to today, so final numbers cover the whole grant. Sections from a scan say when archives have finished since it. Suitable for a progress report or data management plan. |
| `lifecycle` | Grants past their end date that still have data on the HPC or jobs logged to them; remote holdings by grant |
| `stale [--older-than 180d] [--grant G] [--top N]` | The largest directories with no file modified within the period. Only the topmost stale directory of each subtree is listed, since everything inside it is stale too. A policy with `unchanged_for` turns such directories into proposals (§16). |
| `reclaimable` | Caches and software environments with suggested commands, such as `apptainer cache clean`, `conda clean --all`, or `apptainer build` to turn a sandbox into one SIF file |

`stomp changes` (§11) prints in the same formats, so it isn't repeated as a report.

- **Formats:** terminal table, CSV, Markdown and JSON. HTML (one self-contained file with inline SVG charts) is deferred.
- **CSV rows are flat and machine-friendly:** raw byte counts and RFC 3339 times. The table and Markdown formats are for people.
- Every report states the measurement and sync times, the backend used and any caveats, such as possible lag in the Ceph totals.

## 19. TUI

- An `ncdu`-style tree browser built with `ratatui` on the same core library as the CLI.
- **On Ceph, sizes come live from the directory totals,** so browsing needs no scan.
- **Display:** colored by grant; excluded directories and archived directories are marked.
- **Actions:** assign a directory to a grant (edits the config), estimate, archive, restore and view the change history.
- **A compute panel** shows recent jobs per grant.

### 19.1 As built (M7)

`stomp tui [DIR]` starts at DIR, at the root if there is one, or at a list of the roots if there are several. It needs a terminal.

**What it shows.**

- **One row per entry** of the current directory, largest first, ncdu-style: size, a bar relative to the largest entry, file count, age (newest change), grant, and name.
- **Sorting.** `s` cycles through size, name, file count, and oldest first. Hidden entries are shown.
- **Colors.** Names and grant labels are colored per grant, in config order. Excluded directories and archive data are grey.
- **Marks:**
  - `[environment]`, `[cache]`, `[excluded]`, `[stomp data]`
  - `[archived ID]`, or `[archiving ID]` while unfinished
  - `[archive data]`
  - `[grants A, B]` for directories that contain grant directories
  - `[proposed #N]` for open archive proposals (§16)
- **The header** shows the path, the grant of the current directory and its end date, and the total. It also shows whether sizes are live (CephFS) or still being measured, the quota where the filesystem has one, and the sort.

**How sizes are measured.**

- A background thread measures each listed directory with the filesystem backend. On CephFS that's instant; elsewhere it's a walk.
- The same pass checks for environment markers and archives.
- Results are cached for the session, so returning to a directory is instant. `R` measures again.
- Moving on supersedes the measurements still waiting for the previous directory.

**Keys.**

- **Moving:** arrows or `h`/`j`/`k`/`l`, `Enter`, `Backspace`, `PgUp`/`PgDn`/`Home`/`End`.
- **Info (`i`):** grant, size, marks, proposal and archive location. It also shows the directory's size in up to 8 past scans and, for roots and grant directories, the last 10 days of usage samples.
- **Compute (`c`):** a panel of jobs, CPU-hours and GPU-hours per grant over the last 30 days. The current grant is highlighted, and the panel says when accounting was last imported.
- **Grant (`g`) (D35):** pick a grant to add the directory to, or remove it if it's listed as a grant directory.
  - **The edit keeps the file's comments and layout** (`toml_edit`).
  - **It's checked first.** The result is loaded as a config, so overlaps and nesting are refused with the validator's own message.
  - **The previous file is saved** as `config.toml.bak`, and the TUI carries on with the new config.
- **Estimate, archive and restore (`e`, `a`, `r`) (D34):**
  - **stomp's own checks run first.** A directory that can't be archived (an environment, one holding other grants, already archived) gets an explanation without leaving the screen.
  - **Then the browser steps aside.** It runs `stomp archive DIR --estimate`, `stomp archive DIR` or `stomp restore DIR` as the CLI would, with the same output, questions and task submission, and returns on Enter.
- **`?`** shows help; **`q`** or **Esc** quits.

**Terminal handling.** stomp never asks the terminal for its cursor position, since that question can time out over a slow SSH connection. A panic restores the terminal before its message is printed.

**Selection and key hints.** A fresh listing keeps the top row selected while sizes arrive, until the user moves; after that the selection follows its entry through re-sorts. (Otherwise a file, whose size is known at once, starts out selected and drifts down as directories are measured.) The footer shows as many key hints as fit the width, always ending with `? help  q quit`.

## 20. Implementation

### 20.1 Structure

Start as a single crate whose modules have clear boundaries. Split it into a workspace later if that becomes useful.

| Module | Contents |
|--------|----------|
| `config` | Parsing, validation, path expansion |
| `grants` | Storage attribution and usage samples |
| `fs` | Backend interface, Ceph and POSIX backends, parallel walker |
| `catalog` | SQLite schema, migrations, ingest, retention |
| `scan` | Scanning, totals, excluded-directory detection, checkpoints |
| `archive` | Chunk writer and reader, manifests, the per-chunk steps and journal, restore |
| `dest` | Destination interface; `dest::sftp` (OpenSSH subprocesses) and `dest::s3` (AWS SDK, behind the `s3` feature) |
| `compute` | `jobs log` records, `sacct` sync and parsing, attribution, metrics |
| `slurm` | Task script generation, submission, tracking, signal handling, scheduling |
| `policy` | Rule matching and proposals |
| `report` | Renderers for each output format |
| `cli` | `clap` command definitions and output |
| `tui` | Later |

### 20.2 Dependencies (planned)

| Purpose | Crates |
|---------|--------|
| Command line and config | `clap`, `serde`, `toml` |
| Catalog | `rusqlite` (bundled SQLite) |
| Archives | `tar`, `zstd` (multithreaded), `blake3`, `sha2` |
| Filesystem | `xattr`, `rustix` (statfs, statx), `rayon` or a custom work-stealing walker |
| S3 | A built-in SigV4 client over `ureq` (rustls), with `md-5` for Content-MD5 (D25) |
| Dates and durations | `jiff` |
| Errors and logging | `thiserror`, `anyhow`, `tracing` |
| Progress output | `indicatif` |
| TUI | `ratatui` (with `crossterm`); `toml_edit` for editing the config |

### 20.3 Build and distribution

- Built as a static binary for `x86_64-unknown-linux-musl`, so it doesn't depend on the cluster's glibc version. aarch64 can be added if needed.
- `scripts/build-static.sh` builds it with the local musl toolchain when one is installed, or otherwise in Docker with the official Rust Alpine image, and refuses to ship a dynamically linked result. `--test` runs the test suite against musl first.
- **The static build uses mimalloc** (D36). musl's own allocator serializes threads, and parallel directory walks ran about 7× slower than the glibc build (`du` over 526k files: 0.97 s vs 0.11 s). With mimalloc they're close (0.14 s), as is a full scan. Archiving is about 20% slower than glibc (3.0 s vs 2.4 s for 444 MiB), which CephFS I/O will hide.
- Checked on CentOS 7 (glibc 2.17), Rocky Linux 8 and 9 containers: init, usage, du, scan, archive, verify, restore, and an HTTPS request to Spaces (DNS, TLS and signing work with the built-in root certificates).
- Users copy one file into `~/bin`.
- **External programs are needed only for the features that use them:** OpenSSH `ssh`/`sftp` for SFTP destinations, and Slurm's `sbatch`, `squeue`, `sacct` and `scrontab` for tasks and compute attribution.

## 21. Testing

- **Unit tests:**
  - storage attribution arithmetic
  - config validation (overlap, nesting and destination rules)
  - policy matching
  - TB-month calculation
  - retention
  - compute attribution precedence and metric calculation
- **Backends:**
  - Ceph, through a fake attribute provider that can simulate stale totals
  - POSIX, on temporary directories
- **Archive round-trip property tests:**
  - Generate random trees with non-UTF-8 names, newlines in names, very long paths, symlinks, hard links, empty and sparse files, and unreadable files.
  - Archive each tree, restore it, and compare content and metadata.
- **Crash and resume tests:** kill the process at every step of §12.4, then assert that invariants I1–I4 hold after resuming.
- **Destinations:**
  - **SFTP:** run against a local `sshd` in a container in two setups: shell allowed, and SFTP-only (`ForceCommand internal-sftp`).
  - **S3:** run against MinIO in a container, covering resuming an interrupted multipart upload, rejection of a deliberately wrong checksum, and range downloads.
  - **Cold storage:** test the thaw flow with a mocked S3 client.
- **Compute:**
  - Parse saved `sacct` output from several Slurm versions, including arrays, jobs with GPU TRES and still-running jobs.
  - Check that `stomp jobs log` exits 0 when there is no config, when the inbox can't be written, when it runs outside a job, and when the grant is unknown.
- **Manual acceptance on the cluster:**
  - `doctor` and `doctor --submit`
  - `usage` matches `getfattr`
  - an incremental scan after a known change
  - an archive and restore of a test directory
  - an SFTP round trip and a DigitalOcean Spaces round trip
  - a job logged with `stomp jobs log` appearing in `report compute`

## 22. Roadmap

| Milestone | Contents | Value |
|-----------|----------|-------|
| **M1 Usage** ✅ | config, `init`, `doctor`, storage attribution, `usage`, `du`, usage samples, basic CSV and table report, **`stomp jobs log`** (writes records to the inbox only) | Per-grant storage numbers immediately; job tags start accumulating |
| **M2 Scan and compute** ✅ | walker, excluded directories, catalog and ingest, incremental scans, task submission, `tasks`, `changes`, **`jobs sync`, working-directory inference, `report compute`**, reports (`grant`, `stale`, `reclaimable`, `lifecycle`, `history` with TB-months). Deferred from M2: HTML reports, `track_files`, `jobs log --name`. | History, change tracking, finding stale data, compute per grant |
| **M3 Local archive** ✅ | chunk format, manifests, the per-chunk steps and journal, invariants, `--estimate`, local restore, `archives list/show/verify` | Compression in place |
| **M4 Remote destinations** ✅ | destination interface, SFTP, S3 (standard storage classes), verification methods, partial restore, `archives move` (which also covers the planned `push`), remote reports, destination checks in `doctor` | Moving data off the HPC |
| **M5 Automation** ✅ | policies, `candidates` and `apply`, `schedule` | Hands-off upkeep |
| **M6 Cold storage** (deferred) | S3 Glacier and Deep Archive: thaw requests, two-phase restores, cost estimates | Cheap long-term archives |
| **M7 TUI** ✅ | Interactive browser | Convenience |

## 23. Decisions

| ID | Decision | Rationale |
|----|----------|-----------|
| D1 | Runs only as the user, with no background service | Not an admin tool; colleagues must be able to adopt it alone |
| D2 | Grants are mapped by **directory**, and directories never overlap | Matches how the data is actually organized; on Ceph it makes attribution exact and instant |
| D3 | Ceph backend that reads directory totals, with a POSIX fallback | Instant usage and incremental scans on the target cluster, while still working elsewhere |
| D4 | The catalog stores per-directory totals, not per-file rows (per-file tracking is opt-in) | Keeps the catalog inside the user's quota with tens of millions of files |
| D5 | SQLite with a single writer, plus an inbox for task results and job logs | Safe on a network filesystem; tasks, job scripts and the CLI never conflict |
| D6 | SFTP through OpenSSH subprocesses rather than an embedded SSH library | Reuses the user's existing SSH setup; nothing cryptographic in our binary |
| D7 | Chunked `.tar.zst` archives with the same layout everywhere, restorable with standard tools | Partial restores, bounded extra space, no lock-in |
| D8 | Policies propose by default | Nothing destructive happens unless the user explicitly asks for it |
| D9 | Heavy commands submit Slurm tasks by default | Keeps load off login nodes |
| D10 | Files owned by other users are skipped by default | Never delete a collaborator's files in a shared directory |
| D11 | Environments and caches are counted in usage but never scanned or archived | Sandboxes hold millions of files that can be rebuilt; caches can be cleaned instead |
| D12 | Destinations are a named list of pluggable types (`sftp`, `s3`) | One archive format and one restore path; users can keep several destinations |
| D13 | *(Replaced by D25.)* S3 through the embedded AWS SDK | — |
| D14 | Compute jobs are attributed by explicit log first, then by working directory | Most jobs are attributed with no effort, explicit logging overrides it, and history can be filled in from `sacct` |
| D15 | `stomp jobs log` never fails the user's job | A tracking tool must never cost someone a compute run |
| D16 | Accounting records are copied into the catalog | Sites purge old `sacct` records |
| D17 | stomp's own Slurm work is called **tasks**; **jobs** means the user's compute jobs | Avoids an ambiguous `stomp jobs` command |
| D18 | Scans store a histogram per modification month, not relative age bands | Reused subtrees stay correct as their files age; age bands are computed at report time |
| D19 | The scan result file commits each unit without syncing to disk | About 20 times faster, and it avoids metadata-server round trips on CephFS; a killed process loses nothing, and an OS crash only means redoing a few units |
| D20 | `stomp changes` compares grant totals by default; `--dirs` compares scans | The per-grant view is instant on Ceph and needs no scan; the directory view needs two scans |
| D21 | An archive's plan is fixed at the start and saved with it | Resuming is deterministic; files created during archiving simply stay in place |
| D22 | Deletion checks every original in a chunk before removing any; hard links are checked without ctime | Removing one hard link changes the shared inode's ctime, which would otherwise make the other links look modified |
| D23 | Before deleting originals, each chunk is fully decompressed and every file's BLAKE3 compared | Verifying the chunk's SHA-256 alone would only prove the chunk wasn't damaged after writing, not that it holds the right data |
| D24 | A restore treats files whose size and mtime match as already restored | An interrupted restore can be rerun without reporting its own work as conflicts |
| D25 | S3 through a small built-in SigV4 client, not the AWS SDK | Synchronous like the rest of stomp; a small static binary; never sends the SDK's default checksum headers, which some S3-compatible services reject; full control over retries and multipart resume. The cost: no AWS SSO or assume-role, which Spaces doesn't need. Revisit for Glacier (M6). |
| D26 | Every S3 request is signed with its body's SHA-256 and carries a Content-MD5; objects record their SHA-256 | The server rejects anything damaged in transit, and `doctor` proves it does |
| D27 | SFTP verification falls back automatically from `sha256sum` over SSH to reading back | Works with full-shell and SFTP-only servers without configuration |
| D28 | Remote archives keep their records in the archived directory; only chunks leave | Listing, planning a restore and `show` work offline, and the directory explains itself |
| D29 | Without a `path`, policies propose only grant data, and nothing hidden | Software, settings and reference data look "unchanged" but are used daily; the tool's purpose is grant data |
| D30 | `unchanged_for` requires both mtime and ctime to be old | Copied, synced or restored data keeps old mtimes; it has just arrived and shouldn't be archived straight back |
| D31 | One small daily job; the weekly scan is a task it submits | Short jobs start quickly; scans get the resources and checkpointing they need |
| D32 | Automatic archiving needs an age condition, waits after the first proposal (7 days by default), checks again, and starts at most 3 a run | Time to notice and dismiss; nothing changing is archived; no queue floods |
| D33 | The daily run writes the catalog briefly, like an interactive command | The usage sample and accounting import are seconds of work; routing them through the inbox would only add delay |
| D34 | TUI actions step out to the CLI's own commands | One implementation of each check, question and message; nothing destructive is reimplemented |
| D35 | The TUI edits the config in place, validating first and keeping a `.bak` | Users' comments and layout survive; a bad edit is never saved |
| D36 | The static musl build uses mimalloc as its allocator | musl's allocator made parallel walks about 7× slower; glibc builds keep the system allocator |
| D37 | A remote copy kept after a restore keeps its records in the data directory, the manifest only up to 64 MiB | The copy stays listable, checkable and restorable; huge manifests are fetched when needed instead of filling the user's quota |
| D38 | Restoring all of an unfinished archive undoes it | The note's "to put everything back" advice must leave a directory that can be archived again, with nothing stranded at the destination |
| D39 | Remote deletions remove the manifest first and abort unfinished multipart uploads | An interrupted deletion never looks like a complete archive, and abandoned uploads aren't stored (and billed) forever |

## 24. Open questions

- **Q1. NAS access:** stomp now handles both cases (D27), and `stomp doctor` reports which applies.
  - Still to confirm: the NAS hostname and base path, whether the NAS has a quota, and whether the NAS is itself backed up. After archiving, the NAS copy is the only copy.
  - Also to confirm: that the `sftp` batch commands behave as tested against real OpenSSH (§13.5).
- **Q2. "File management" scope:** this draft covers `du`, `find`, reclaimable caches and moving directories between grants (by editing the config). Are deleting, moving or deduplicating files also wanted?
- **Q3. CPU architecture:** the cluster's `uname -m` output. x86_64 is assumed.
- **Q5. Accounting fields:** does the site record GPUs and billing units in `AllocTRES`? Check with `sacct -X -S now-7days -o JobID,AllocTRES%80 | head`.

### Resolved

- **Q4. S3 provider:** DigitalOcean Spaces (§13.3.7). Its keys are long-lived, so credential expiry isn't a concern. Glacier is wanted for long-term storage eventually but is deferred (M6, §13.3.5). Whether compute nodes can reach Spaces is still to be checked with `stomp doctor --submit`.

### Deferred

- **Q6. Splitting a job across grants:** a good idea, but not needed so far (§25).

## 25. Future ideas

- Offer to convert Apptainer sandboxes to SIF images (`apptainer build x.sif sandbox/`). One file replaces millions, which saves both space and load on the metadata server.
- Duplicate detection within and across grants.
- Shared grant definitions for collaborators, so a lab or group can see combined storage and compute per grant across users.
- Grant mappings with effective dates, so that remapping a directory doesn't rewrite its history.
- Report delivery by email or chat.
- An Open OnDemand app built on the same core.
- More transports: Globus, or `rclone`, which would add Google Drive, OneDrive and Box.
- Attribution without stomp in job scripts: `#SBATCH --comment=stomp:GRANT-A`, read back through `sacct`. This works only if the site stores job comments (`AccountingStoreFlags=job_comment`).
- A seekable zstd format, so a single file can be restored from S3 with a byte-range download instead of fetching the whole chunk.
- Splitting one job across grants (`stomp jobs log GRANT-A=0.5 GRANT-B=0.5`). Deferred from Q6 until there's a real need.
