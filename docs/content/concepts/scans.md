+++
title = "Scans and the catalog"
description = "What a detailed scan records, how later scans skip unchanged directories, and where results go."
weight = 30
+++

A **scan** walks every root in a Slurm job and records, for each directory,
what CephFS's running totals can't tell you: how old the data is, what sizes
and types of files it holds, and which directories are software environments
or caches. Reports and policies read the newest scan.

## What a scan records

For each directory (not each file):

- its size and file count, both directly inside it and in its whole subtree
- the newest and oldest modification time, and the newest change time
- how many files and bytes were last modified in each **month**
- a size breakdown: files under 4 KiB, 4 KiB–1 MiB, 1–100 MiB, 100 MiB–10 GiB,
  and over 10 GiB
- whether it's a root, a grant directory, or an excluded directory
  (environment, cache, `scan.exclude`, stomp's own data)

For each grant and root, a full scan that wasn't interrupted also records the
file types (by extension) and the 100 largest files.

**Why months?** Reports show age bands like "90 days–1 year". Those bands
would go stale if they were stored, because files keep getting older after
the scan. A count per calendar month stays correct, and reports turn it into
bands when they run.

**Why not files?** A home directory can hold tens of millions of files. A row
per file would make the catalog gigabytes in size, and it lives in your own
quota. Per-directory totals are enough for every report, and small
directories (under 1 MiB **and** under 100 files, by default) are folded into
their parent to keep it small. `stomp doctor` shows the catalog's size.

**What isn't walked.** Excluded directories are recorded with their size but
not entered. Symlinks aren't followed, other filesystems aren't entered,
`.snap` directories are skipped, and unreadable directories are recorded with
the error.

## Running a scan

```sh
stomp scan
```

Outside a job, this submits a [Slurm task](@/concepts/slurm-tasks.md) using
your `[slurm]` settings and returns. `stomp tasks` shows its progress and,
once it's done, imports its results (so does any report).

A scan examines at most `scan.max_threads` files at once (8 by default), and
never more than the CPUs Slurm gave the task. The metadata server is shared
by everyone on the cluster, and an unthrottled walk of millions of files can
slow it down for all of them.

With the [daily run](@/concepts/policies.md#the-daily-run) enabled, a scan is
submitted once a week for you.

## Incremental scans on CephFS

On CephFS, every directory carries the newest change time of anything inside
it (`ceph.dir.rctime`). If a subtree's change time is older than when the
previous scan started, nothing in it has changed, so stomp skips it and
reuses the previous scan's numbers. A second scan of a mostly quiet home
directory takes a fraction of the first.

Two safeguards keep this honest:

- **A safety margin.** CephFS updates change times lazily, so a subtree is only
  skipped if it's older than the previous scan's start minus
  `scan.rctime_margin` (1 hour by default).
- **Periodic full scans.** Every `scan.full_every` (30 days by default), and
  whenever you run `stomp scan --full`, every directory is walked.

On other filesystems every scan walks everything.

## Interrupted scans

Scans save progress as they go. The directories two levels below each root
are **work units**, and each is saved as soon as it's done.

- Five minutes before the task's time limit, Slurm signals it, and the scan
  stops starting new units and exits marked incomplete.
- With `slurm.auto_resubmit` on (the default), stomp resubmits it, and the new
  job continues where the old one stopped, up to 5 times.
- You can also continue by hand with `stomp scan --resume`.

A resumed scan lost the file-type and largest-file statistics gathered before
the interruption, so those come from the newest full scan that ran without
one. Reports say which scan they used.

## The catalog

Results go into the **catalog**, a SQLite database at
`~/.local/share/stomp/catalog.db`. Only commands you run interactively write
to it. A task writes its results to `~/.local/share/stomp/inbox/`, and the
next command that reads the catalog imports them. SQLite's file locking isn't
reliable on network filesystems like CephFS, so this keeps the database safe
however many tasks and jobs are running.

**How long scans are kept.** Per-directory detail is kept for the newest 4
scans, plus one scan per month for the last 12 months. Usage samples, job
records and archive records are kept forever; they're small.

See [Files and directories](@/reference/files.md) for everything in the data
directory.
