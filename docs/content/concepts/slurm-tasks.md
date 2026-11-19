+++
title = "Slurm tasks"
description = "How stomp runs heavy work as your own Slurm jobs, and continues when one runs out of time."
weight = 40
+++

Login nodes are shared, and walking millions of files or compressing
terabytes on one slows it down for everybody. So stomp sends heavy work to
Slurm as ordinary jobs under your account. stomp calls these **tasks**, to
keep them apart from your own compute **jobs**, which it attributes to grants.

## Which commands submit tasks

| Heavy: submit a task | Cheap: run right away |
|----------------------|-----------------------|
| `stomp scan` | `stomp usage`, `stomp du`, `stomp tui` |
| `stomp archive` | `stomp report …`, `stomp changes` |
| `stomp restore` | `stomp jobs …`, `stomp tasks …` |
| `stomp archives move` | `stomp archives`, `show`, `verify` |
| `stomp apply` (one task per directory) | `stomp candidates`, `stomp doctor` |
| `stomp doctor --submit` (1 CPU, 1 GB, 10 minutes) | `stomp init`, `stomp schedule …` |

A heavy command:

- **submits a task and returns**, printing the task's ID, when you run it
  from a login node;
- **runs directly** when it's already inside a Slurm job (`SLURM_JOB_ID` is
  set), for example in an interactive `salloc` session or your own batch
  script;
- **runs in the foreground** with `--here`. On a login node, keep that to
  small directories.

```sh
stomp scan
```

```text
Submitted scan task scan-20261118-180506-0839 as Slurm job 4810233.
Follow it with `stomp tasks`; once it's done, that (or any report) imports the results.
```

## Resources

Tasks use the `[slurm]` section of your config:

```toml
[slurm]
account   = "lab-rivera"
partition = "standard"
time      = "08:00:00"
cpus      = 8
mem       = "16G"
```

Any of these can be overridden for one command:

```sh
stomp archive ~/marsh_mapping --time 24:00:00 --mem 32G
```

Keys you leave out are left to Slurm's defaults, except `cpus`, which defaults
to `scan.max_threads` (8). Archiving a directory with millions of files needs
memory for the file list, about 1.5 GB per 10 million files.

## Following tasks

```sh
stomp tasks
```

```text
TASK                        STATUS   SLURM JOB   UPDATED            PROGRESS
scan-20261118-180506-0839   done     4810233     2026-11-18 18:05   1,385 files, 1.7 TiB
```

- `stomp tasks show ID` gives the details, including where the job script
  and log are.
- `stomp tasks cancel ID` cancels the task's Slurm job and says how to go on:
  resume a scan or archive, undo an archive, or run a restore again.

STATUS is `pending` or `running` while Slurm has the job, then `done`,
`partial` (a restore that left existing files alone), `incomplete` (stopped
early; resume it), `failed` or `cancelled`. `stomp tasks` also imports the
results of tasks that have finished.

Each task has a directory in `~/.local/share/stomp/tasks/<ID>/` holding the
`task.sh` stomp submitted, the job's output, and its progress. You can read
the script, and rerun it with `sbatch` if you need to.

## Running out of time

Every task asks Slurm for a warning five minutes before its time limit
(`--signal=B:USR1@300`). On that signal, or if the job is cancelled, the task
finishes the step it's on, saves its progress, and stops cleanly:

- **Scans** save each finished work unit, and resume from there.
- **Archives, restores and moves** keep a journal of every step, so a resumed
  run never repeats a deletion or upload, and never loses data.

When it stopped for the time limit and `slurm.auto_resubmit` is on (the
default), stomp then resubmits the task to continue, up to 5 times. Otherwise,
or after `stomp tasks cancel`, it stays stopped: continue it yourself with
`stomp scan --resume` or `stomp archive --resume DIR`. A cancelled task never
resubmits itself.

## Where results go

Tasks never write the catalog directly. They write their results to
`~/.local/share/stomp/inbox/`, and `stomp tasks`, or the next command that
reads the catalog (a report, `stomp usage`), imports them. `stomp ingest`
imports them explicitly.

## Tasks in your compute reports

Tasks use compute too, and stomp attributes them like any other job: an
archive or restore task counts toward the grant whose data it handles, and
scans count toward `stomp`, a row for stomp's own overhead. So
`stomp report compute` shows what keeping your storage tidy costs.
