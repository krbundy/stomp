+++
title = "Policies and the daily run"
description = "Rules that propose directories to archive, and the small daily job that keeps everything current."
weight = 70
+++

Deciding what to archive by hand works for one finished project. For years of
data across several grants, you can write **policies**: rules that look at
the newest scan and propose directories to archive. Nothing is archived
unless you say so.

## Policies

```toml
[[policy]]
name        = "ended-grants"
match       = { grant_ended_for = "90d" }
destination = "nas"

[[policy]]
name   = "old-small-files"
match  = { unchanged_for = "1y", min_files = 10000, max_avg_file_size = "1MiB" }
action = "apply"
wait   = "14d"
```

The first proposes the data of every grant that ended at least 90 days ago,
to go to the NAS. The second proposes directories nobody has touched for a
year that hold at least 10,000 files averaging 1 MiB or less, and archives
them automatically two weeks after first proposing them.

The conditions (all must hold) are `grant`, `path`, `grant_ended_for`,
`unchanged_for`, `min_bytes`, `min_files` and `max_avg_file_size`; see the
[configuration reference](@/reference/configuration.md#policy).

## What gets proposed

- **The outermost matching directory.** If `~/marsh_mapping` matches, its
  subdirectories aren't proposed separately; archiving the top covers them.
  When two policies propose the same directory, the first in the config wins.
- **Only grant data, unless you give a `path`.** Without a `path`, policies
  look only inside grant directories, and never under hidden directories.
  Software, settings and reference data are often read every day without being
  modified, so age alone mustn't put `~/bin`, `~/.conda` or a shared genome
  index up for archiving. To include other data, write a policy with a `path`
  glob, such as `path = "~/scratch/*"`.
- **Never proposed:** roots, directories containing another grant's
  directories, environments, caches and `scan.exclude` matches, stomp's own
  data, and anything already archived.
- **Environments and caches don't count.** They stay in place when their parent
  is archived, so they're left out of a proposal's size and conditions.

### "Unchanged" means really unchanged

`unchanged_for` requires both the newest **modification** time (mtime) and the
newest **change** time (ctime) in the directory to be older than the period.
Copying with `cp -p` or `rsync -a`, or restoring an archive, keeps old mtimes
but sets fresh ctimes. Data that has just arrived therefore isn't proposed
straight away, however old its timestamps look.

`stomp report stale` uses modification times only, so it may list
directories a policy won't propose yet.

## Proposals

```sh
stomp candidates
```

lists the open proposals with a stable number, each one's size, its file
count before and after archiving, roughly how much space it would free, where
it would go, and why it matched. The proposals are updated every time you run
it: new matches are added, ones that no longer match are removed, and the rest
keep their number.

- **Space freed** for a remote destination is the whole size. For a local
  archive it's the part compression saves, estimated by compressing a small
  sample: the first MiB of 16 files picked in proportion to size.
- **`stomp candidates dismiss ID`** stops proposing a directory for good.
- **`stomp apply ID...`** (or `--all`) archives proposals, each as its own
  Slurm task, with the usual confirmation.

Before applying, stomp checks each proposal again: the policy must still exist
and its grant and path conditions still hold, the directory must pass the
archive checks, and on CephFS its live size, file count and change time must
still meet the policy's conditions. A proposal that fails is skipped with the
reason.

## Automatic archiving

`action = "apply"` lets the daily run archive a policy's proposals without
asking. Three safeguards apply:

- **An age condition is required**: `grant_ended_for` or `unchanged_for`. A
  rule based only on size could archive data you're actively using.
- **It waits.** A proposal is archived only `wait` (7 days by default) after it
  was first proposed. `stomp usage` reminds you when proposals are open, so
  you have time to see them and dismiss any you want to keep.
- **It's capped.** Each daily run starts at most 3 automatic archives, oldest
  proposals first.

Each is checked again before it starts, exactly as with `stomp apply`, and all
the [archive rules](@/concepts/archives.md#the-rules) still hold.

## The daily run

```sh
stomp schedule enable
```

sets up one small Slurm job (1 CPU, 2 GB, 30 minutes, using your `[slurm]`
account and partition) that runs once a day. Each run:

1. imports finished task results and job logs;
2. records a usage sample, if every root is on CephFS (elsewhere that would
   mean walking every file, so it skips it and says so);
3. imports job accounting from `sacct`;
4. submits a scan task if the last scan started 6½ days ago or more, or
   resumes one that stopped early, unless one is already queued or running;
5. checks the policies against the newest scan and estimates compression for
   new proposals;
6. starts archive tasks for `apply` proposals whose wait has passed (at most
   3);
7. keeps the newest 30 run logs.

**How it's scheduled.** If your site enables Slurm's `scrontab`, stomp adds an
entry between marker comments and leaves the rest of your crontab alone.
Otherwise it uses a job submitted with `sbatch --begin` that queues the next
day's run before doing its own work, so one failed run doesn't break the
chain. Choose with `--mode scrontab` or `--mode task`.

**When.** By default, at a time between 02:00 and 05:00 that's different for
each user, so a lab's runs don't all start at once. Choose with
`--at HH:MM`.

**Checking on it.** `stomp schedule status` shows the mode, the next queued
run, and how the last run went, line by line. It reports a problem if the
entry or queued job is missing, or if no run has finished in two days;
`stomp doctor` reports the same. Logs are in
`~/.local/share/stomp/schedule/logs/`.

`stomp schedule disable` turns it off, and `stomp schedule run` does a run's
work right now, in the foreground.
