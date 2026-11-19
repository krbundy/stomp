+++
title = "Commands"
description = "Every stomp command and flag."
weight = 10
+++

Run `stomp help` or `stomp <command> --help` for the same information in the
terminal. This page groups the commands the way you'd use them:

| Command | What it does | Runs |
|---------|--------------|------|
| [`stomp init`](#stomp-init) | Write a starter config from your directory layout | here |
| [`stomp doctor`](#stomp-doctor) | Check the config, environment and destinations | here, or a task with `--submit` |
| [`stomp usage`](#stomp-usage) | Storage per grant, saved to the history | here |
| [`stomp du`](#stomp-du) | Directory sizes with their grants | here |
| [`stomp scan`](#stomp-scan) | Detailed scan: ages, file types, environments | task |
| [`stomp changes`](#stomp-changes) | How storage changed, per grant or per directory | here |
| [`stomp report`](#stomp-report) | Reports for budgets and planning | here |
| [`stomp jobs`](#stomp-jobs) | Attribute Slurm jobs to grants | here |
| [`stomp tasks`](#stomp-tasks) | stomp's own Slurm tasks | here |
| [`stomp archive`](#stomp-archive) | Pack a directory into verified chunks and remove the originals | task |
| [`stomp archives`](#stomp-archives) | List, inspect, verify and move archives | here; `move` is a task |
| [`stomp restore`](#stomp-restore) | Put archived files back | task |
| [`stomp candidates`](#stomp-candidates) | Directories your policies propose to archive | here |
| [`stomp apply`](#stomp-apply) | Archive proposed directories | tasks |
| [`stomp tui`](#stomp-tui) | Browse interactively | here |
| [`stomp schedule`](#stomp-schedule) | The daily run | here; runs are Slurm jobs |
| [`stomp ingest`](#stomp-ingest) | Import finished task results now | here |

"Task" means the command submits a Slurm job and returns; see
[Slurm tasks](@/concepts/slurm-tasks.md) and [task options](#task-options) below.

## Options for every command {#global-options}

| Option | Meaning |
|--------|---------|
| `--config <PATH>` | Use this config file instead of `~/.config/stomp/config.toml` (or `$XDG_CONFIG_HOME/stomp/config.toml`). |
| `--json` | Print JSON instead of a table. The same as `--format json` where a command has `--format`. |
| `-h`, `--help` | Show help. `-h` gives a summary, `--help` the full text. |
| `-V`, `--version` | Print stomp's version. |

### Output formats {#output-formats}

Commands that print tables accept `--format`:

| Value | Output |
|-------|--------|
| `table` | Aligned columns for the terminal (the default) |
| `csv` | One row per line with raw numbers (bytes, not "1.2 TiB"), for spreadsheets |
| `json` | Structured data, for scripts |
| `md` | A Markdown table, for pasting into documents, issues and wikis |

See [Sizes, durations and output](@/reference/formats.md) for details.

### Task options {#task-options}

Commands that do heavy work (`scan`, `archive`, `restore`, `archives move`,
`apply`, and `doctor --submit`) submit a Slurm task by default. Inside a Slurm
job (when `SLURM_JOB_ID` is set) they run directly instead. They accept:

| Option | Meaning |
|--------|---------|
| `--here` | Run in the foreground instead of submitting a task. On a shared login node, keep this to small jobs. |
| `--time <TIME>` | Time limit for the task, e.g. `12:00:00`. Overrides `slurm.time`. |
| `--cpus <N>` | CPUs for the task. Overrides `slurm.cpus`. |
| `--mem <SIZE>` | Memory for the task, e.g. `16G`. Overrides `slurm.mem`. |
| `--partition <PARTITION>` | Slurm partition. Overrides `slurm.partition`. |
| `--account <ACCOUNT>` | Slurm account. Overrides `slurm.account`. |

### Exit codes {#exit-codes}

| Code | Meaning |
|------|---------|
| 0 | Success |
| 1 | Error, or `doctor` / `schedule status` found a problem |
| 2 | Invalid arguments, or the config has errors |
| 3 | Partial success: a scan, archive, restore or move stopped before finishing (continue it as the message says), or a restore left conflicts |

`stomp jobs log` always exits 0 unless `--strict` is given, so it can never
fail a job.

## stomp init

Writes a starter config that lists your top-level directories, with a
commented-out `[[grant]]` block for each, ready to fill in.

```text
stomp init [--root <DIR>]... [--print]
```

| Option | Meaning |
|--------|---------|
| `--root <DIR>` | A top-level directory to manage. Repeat it for several. Default: `~`. |
| `--print` | Print the config instead of writing it. |

`stomp init` never overwrites an existing config. To start over, move the old
file aside first. See [Set up stomp for your grants](@/tutorials/first-setup.md).

## stomp doctor

Checks the config, stomp's data directory and catalog, the filesystem, the
Slurm commands, the daily schedule, and each destination: it logs in, writes a
test file, and finds out how uploads can be verified.

```text
stomp doctor [--offline] [--submit [task options]]
```

| Option | Meaning |
|--------|---------|
| `--offline` | Skip connecting to destinations. |
| `--submit` | Run the checks in a Slurm task, to see what compute nodes can reach. The task asks for 1 CPU, 1 GB and 10 minutes unless you give task options. The result is in the task's log (`stomp tasks show ID`). |

Each line starts with `ok`, `info`, `warn` or `FAIL`. The command exits 1 if
anything failed.

## stomp usage

Storage per grant, plus each root's total, filesystem and quota, and what's
archived elsewhere. Each run is saved to the usage history.

```text
stomp usage [--no-record] [--format FORMAT]
```

| Option | Meaning |
|--------|---------|
| `--no-record` | Don't save this measurement to the history. |

On CephFS the numbers come from the filesystem's own directory totals and are
instant. Elsewhere stomp walks every file, which can take a while on a big
tree. See [How stomp measures storage](@/concepts/measuring.md).

`stomp report usage` is the same command.

## stomp du

Directory sizes, file counts, last change and grant, like `du` with grants.

```text
stomp du [PATH] [-d N] [--format FORMAT]
```

| Option | Meaning |
|--------|---------|
| `PATH` | The directory to show. Default: the current directory. |
| `-d`, `--depth <N>` | How many levels of subdirectories to list. Default: 1. |

Instant on CephFS; elsewhere it walks the tree.

## stomp scan

A detailed walk of every root that records, for each directory: sizes, file
counts, modification months, a size breakdown, and any environments and caches
inside. Reports such as `stale` and `reclaimable`, `changes --dirs` and the
policies use the newest scan. See [Scans and the catalog](@/concepts/scans.md).

```text
stomp scan [--full] [--resume] [task options]
```

| Option | Meaning |
|--------|---------|
| `--full` | Walk everything instead of skipping unchanged directories (CephFS only skips). |
| `--resume` | Continue the most recent interrupted scan. |

Outside a Slurm job, `stomp scan` submits a task and returns. When it's done,
`stomp tasks` (or any report) imports its results. A scan that reaches
its time limit saves its progress and resubmits itself to continue (up to 5
times; see `slurm.auto_resubmit`).

## stomp changes

How storage changed.

```text
stomp changes [--since AGE|DATE] [--grant ID] [--format FORMAT]
stomp changes --dirs [--since AGE|DATE] [--grant ID] [--depth N] [--top N] [--format FORMAT]
```

| Option | Meaning |
|--------|---------|
| `--since <AGE\|DATE>` | Compare with the newest measurement (or scan, with `--dirs`) at or before this time, e.g. `30d` or `2026-10-01`. Default: the newest one. |
| `--grant <ID>` | Only this grant. |
| `--dirs` | Compare two scans directory by directory, instead of grant totals. |
| `--depth <N>` | With `--dirs`: how many levels below each root or grant directory. Default: 2. |
| `--top <N>` | With `--dirs`: how many changes to show. Default: 20. |

Without `--dirs`, stomp takes a fresh measurement and compares it with a
recorded one. The fresh measurement isn't saved, so running it again compares
with the same baseline. With `--dirs` it needs at least two scans.

## stomp report

Reports for grant budgets, progress reports and planning. Every report accepts
`--format table|csv|json|md`.

```text
stomp report <REPORT> [options]
```

### stomp report usage

The same as [`stomp usage`](#stomp-usage), including `--no-record`.

### stomp report history

Storage per grant at each recorded measurement, with TB-months: storage
added up over time, the unit many storage charges use. A TB here is a decimal
terabyte, 10¹² bytes. Where measurements are more than two days apart, the
TB-months are marked as estimated (`*`), because the storage in between is
interpolated; the daily run records one measurement a day.

| Option | Meaning |
|--------|---------|
| `--from <DATE>` | First day to include (`YYYY-MM-DD`). |
| `--to <DATE>` | Last day to include. |
| `--grant <ID>` | Only this grant. |
| `--tb-months` | Only the TB-months per grant, with costs if a rate is set. In CSV this is the way to get TB-months into a spreadsheet. |

With `--format json`, the output is `{"measurements": [...], "tb_months": [...]}`.

### stomp report compute

Jobs, CPU-hours, GPU-hours and billing hours per grant, with how each job was
attributed and, if rates are set, an estimated cost.

| Option | Meaning |
|--------|---------|
| `--from <DATE>` | First day to include. Default: 30 days ago. |
| `--to <DATE>` | Last day to include. Default: today. |
| `--by-month` | One row per grant per month. |
| `--no-sync` | Use already-imported accounting without running `sacct`. |

### stomp report grant

```text
stomp report grant <ID> [--from DATE] [--to DATE]
```

One grant's storage and compute together: its PI and dates, the size of each
directory, storage by month with TB-months, data age, file sizes, environments
and caches, file types, the largest files, archives, and compute by month with
a total and, if rates are set, a cost. Suitable for a progress report or a data
management plan; `--format md` pastes straight into a document.

| Option | Meaning |
|--------|---------|
| `--from <DATE>` | First day of the period for storage history and compute. Default: the grant's `start`, or a year ago. |
| `--to <DATE>` | Last day of the period. Default: today. |

The sections on data age, file types and largest files come from the newest
scan; the report says when archives have finished since then.

### stomp report stale

Large directories with no file modified recently. Only the topmost stale
directory of each subtree is listed. Uses the newest scan.

| Option | Meaning |
|--------|---------|
| `--older-than <AGE\|DATE>` | No file modified within this long, or since this date. Default: `180d`. |
| `--grant <ID>` | Only this grant. |
| `--top <N>` | How many directories to show. Default: 20. |

### stomp report reclaimable

Caches and software environments found by the newest scan, with a suggestion
for slimming each one down (for example `pip cache purge`, or `apptainer
build` to turn a sandbox into one SIF file).

### stomp report lifecycle

Each grant's end date and status, its storage on the cluster, what it has
archived elsewhere, and any jobs that ran after it ended.

## stomp jobs

Attribute your Slurm compute jobs to grants. See
[Compute attribution](@/concepts/compute.md).

### stomp jobs log

```text
stomp jobs log <GRANT> [--job <ID>]... [--note <TEXT>] [--strict]
```

Inside a job script, records that the current job belongs to `GRANT`. A job
array is logged once for all its tasks. With `--job`, tags jobs that already
ran instead.

| Option | Meaning |
|--------|---------|
| `--job <ID>` | Tag this job ID instead of the current job. Repeatable. |
| `--note <TEXT>` | A note to store with the record. |
| `--strict` | Treat problems as errors: report them as such, exit 1, and don't record a job for an unknown grant. |

It never makes a job fail: without `--strict`, problems (no config, an
unknown grant, a typo in the arguments) are printed as warnings, the job is
still recorded, and it exits 0. It writes one small file and doesn't touch the
catalog, so it's safe to run from thousands of jobs at once.

### stomp jobs sync

Imports accounting records from `sacct` now. You rarely need it: commands
that show compute import automatically when the last import is more than 15
minutes old, and the daily run imports too. The first import goes back to
`compute.history_start`, or 90 days.

### stomp jobs list

Recent jobs with their grant and how each was attributed, and a NOTE column
when any of them was logged with `--note`.

| Option | Meaning |
|--------|---------|
| `--grant <ID>` | Only this grant. |
| `--unassigned` | Only jobs not attributed to any grant. |
| `--since <AGE\|DATE>` | Jobs since this long ago or this date. Default: `30d`. |
| `--no-sync` | Use already-imported accounting without running `sacct`. |

### stomp jobs retag

```text
stomp jobs retag <GRANT> <JOB>...
```

Assigns jobs to a grant by hand, overriding job logs and working-directory
inference.

### stomp jobs untag

```text
stomp jobs untag <JOB>...
```

Removes jobs' explicit grant tags, so they fall back to working-directory
inference.

## stomp tasks

stomp's own Slurm tasks: scans, archives, restores, moves and doctor checks.

```text
stomp tasks [list]
stomp tasks show <ID>
stomp tasks cancel <ID>
```

| Subcommand | Meaning |
|------------|---------|
| `list` | Every task with its status, Slurm job and progress (the default). It also imports the results of tasks that have finished. |
| `show <ID>` | Details of one task, including where its script and log are. |
| `cancel <ID>` | Cancel the task's Slurm job, and say how to continue it. An archive cancelled partway can be continued with `--resume`, or undone by restoring it. |

A task's status is `submitted`, `pending` or `running` while Slurm has it,
then `done`, `partial` (a restore that left existing files alone), `incomplete`
(stopped early; resume it), `failed` or `cancelled`.

## stomp archive

Packs a directory into compressed, verified chunks and removes the originals.
See [How archives work](@/concepts/archives.md) for the safety rules.

```text
stomp archive <DIR> [--to DEST] [--estimate | --dry-run] [--resume]
                    [--level N] [--chunk-size SIZE] [-y] [task options]
```

| Option | Meaning |
|--------|---------|
| `--to <DEST>` | Where the chunks go: `local` (inside the directory) or a configured destination. Default: `archive.default_destination`, which is `local` unless you set it. |
| `--estimate` | Show the plan and estimate the compressed size by compressing a sample. Changes nothing, and doesn't contact the destination. |
| `--dry-run` | Show the plan without archiving anything. |
| `--resume` | Continue an archive that stopped partway. |
| `--level <N>` | zstd compression level, 1–22. Default: `archive.zstd_level`, 3. |
| `--chunk-size <SIZE>` | Largest chunk, e.g. `50GiB`. Default: `archive.chunk_size`. |
| `-y`, `--yes` | Don't ask for confirmation. |

stomp asks before starting, saying where the chunks will go, then submits a
task. Originals are removed one chunk at a time, only after the chunk has been
verified where it's stored and every file's checksum compared, and only if the
file hasn't changed since it was packed.

It refuses a root, a directory that contains another grant's directories, an
environment, cache or excluded directory, a directory holding stomp's own
data, and a directory that is already archived.

## stomp archives

List, inspect, verify and move existing archives. `ARCHIVE` is an archive ID
or the archived directory.

```text
stomp archives [list]
stomp archives show <ARCHIVE>
stomp archives verify <ARCHIVE> [--deep]
stomp archives move <ARCHIVE> --to <DEST> [-y] [task options]
stomp archives delete <ARCHIVE> [-y]
```

| Subcommand | Meaning |
|------------|---------|
| `list` | Every archive with its state, original and compressed size, file count, creation time, where its chunks are (WHERE: `directory`, a destination, or `—` once nothing remains) and directory (the default). |
| `show <ARCHIVE>` | Details of one archive, starting with where its records and chunks are. Works offline, even for remote archives. |
| `verify <ARCHIVE>` | Checks every chunk's SHA-256 checksum, wherever the chunks are. |
| `verify <ARCHIVE> --deep` | Also decompresses every chunk and checks each file's checksum. Downloads remote chunks to do so. |
| `move <ARCHIVE> --to <DEST>` | Moves the chunks to a destination, or back into the directory with `--to local`. Each chunk is verified at the new place before any copy is removed. |
| `delete <ARCHIVE>` | Deletes the copy an archive keeps at a destination after its files were restored, for good. It asks first, and refuses an archive that still holds the only copy of its files. |

Archive states are `in-progress`, `complete`, `restored` (nothing remains),
`restored-copy-kept` (the files are back and a copy stays at the destination)
and `abandoned` (an unfinished archive that was undone).

If compute nodes can't reach a destination, run `move` with `--here` on a
login or data-transfer node.

## stomp restore

Puts archived files back.

```text
stomp restore <ARCHIVE> [--only PATTERN]... [--to DIR] [--overwrite]
                        [--keep-archive] [--delete-remote] [--dry-run] [-y] [task options]
```

| Option | Meaning |
|--------|---------|
| `ARCHIVE` | An archive ID, or the archived directory. |
| `--only <PATTERN>` | Only paths matching this pattern, relative to the archived directory. Repeatable. A pattern also matches everything under a directory it names. `*` doesn't cross `/`; `**` does. |
| `--to <DIR>` | Restore into this directory instead of the original one. |
| `--overwrite` | Replace files that already exist. Without it, existing files are never touched. |
| `--keep-archive` | Keep the archive even after restoring everything. |
| `--delete-remote` | After restoring everything, also delete the copy at the destination. It's kept by default. |
| `--dry-run` | Show what would be restored: files, chunks (and downloads), what's already in place, conflicts, and what happens to the archive. Changes nothing. |
| `-y`, `--yes` | Don't ask for confirmation. |

Only the chunks that hold the requested files are read or downloaded. `--only`
patterns are checked before a task is submitted, so a pattern that matches
nothing fails at once. A restore can be rerun: files that already match are
counted as restored. When everything is restored into the original
directory with no conflicts (and without `--keep-archive`):

- **a local archive** is removed;
- **a remote archive's** copy stays at the destination, its records move to
  stomp's data directory, and `stomp archives show`, `verify` and `restore`
  keep working on it until you run `stomp archives delete`;
- **an unfinished archive** is undone: what was archived so far is back, the
  rest never left, so the archive (and any uploaded chunks) is removed and the
  directory can be archived again. stomp refuses while the archive's task is
  still queued or running.

## stomp candidates

Directories your [policies](@/concepts/policies.md) propose to archive, with
each one's size, file count before and after, and roughly how much archiving
would free.

```text
stomp candidates [--all] [--format FORMAT]
stomp candidates dismiss <ID>...
```

| Option | Meaning |
|--------|---------|
| `--all` | Also list proposals already applied or dismissed. |
| `dismiss <ID>...` | Stop proposing these directories. |

Policies are checked against the newest scan each time you run it.

## stomp apply

Archives directories proposed by `stomp candidates`.

```text
stomp apply <ID>... | --all [-y] [--here] [task options]
```

| Option | Meaning |
|--------|---------|
| `ID...` | Proposal IDs, as `stomp candidates` lists them. |
| `--all` | Every open proposal. |
| `-y`, `--yes` | Don't ask for confirmation. |
| `--here` | Archive here, one after another, instead of submitting tasks. |

Each proposal is checked again first: it must still exist and still match its
policy, and on CephFS its live size, file count and change time must still meet
the policy's conditions. Each archive then runs as its own Slurm task.

## stomp tui

An interactive, ncdu-style browser over your roots.

```text
stomp tui [DIR]
```

Starts at `DIR`, or at your root (a list of roots if you have several). It
needs a terminal. Press `?` inside for the keys; see
[Browse your storage with stomp tui](@/tutorials/browse.md).

## stomp schedule

A small daily Slurm job that keeps usage, accounting, scans and proposals
current. See [Policies and the daily run](@/concepts/policies.md).

```text
stomp schedule enable [--at HH:MM] [--mode scrontab|task]
stomp schedule status
stomp schedule disable
stomp schedule run
```

| Subcommand | Meaning |
|------------|---------|
| `enable` | Run stomp daily. Running it again changes the time or mode. |
| `enable --at <HH:MM>` | Time of day. Default: a time between 02:00 and 05:00, different for each user. |
| `enable --mode <MODE>` | `scrontab` (Slurm's cron, if the site allows it) or `task` (a job that queues the next one). Default: `scrontab` if available. |
| `status` | How runs are set up, and how the last one went. Exits 1 if something is wrong. |
| `disable` | Remove the scrontab entry or cancel the queued job. |
| `run` | Do a scheduled run's work now, in the foreground. |

## stomp ingest

Imports finished scans, job logs and archive records into the catalog now.
`stomp tasks` and every command that reads the catalog (`usage`, the reports,
`changes`, `jobs`, `archives`, `candidates` and others) do this first, so you
rarely need it.
