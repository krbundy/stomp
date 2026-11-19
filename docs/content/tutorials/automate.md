+++
title = "Automate archiving with policies"
description = "Let rules propose what to archive, review the proposals, and run stomp daily."
weight = 80

[extra]
time = "25 minutes"
needs = "a working config, Slurm, and the archiving tutorial"
+++

Instead of hunting for directories to archive, you can write rules, called
policies, and let stomp propose directories that match them. In this tutorial
you write two policies, review what they propose, archive one proposal, and
turn on a small daily Slurm job that keeps the proposals current and archives
automatically where you have allowed it.

## Write your policies

Policies are `[[policy]]` blocks in your config. Alex adds two to
`~/.config/stomp/config.toml`:

```toml
[[policy]]
name        = "ended-grants"
match       = { grant_ended_for = "90d" }
action      = "apply"
wait        = "30d"
destination = "nas"

[[policy]]
name  = "finished-runs"
match = { path = "~/soil_carbon_runs/2024_*" }
```

The first proposes the data of any grant that ended at least 90 days ago, to be
sent to the NAS (set up in [the previous tutorial](@/tutorials/nas-and-spaces.md)).
Because its action is `apply`, stomp will also archive what it proposes, 30
days after first proposing it. The second proposes the 2024 model runs. It
names no destination, and Alex's config has no `archive.default_destination`,
so they would be archived in place. It only proposes, so nothing happens until
Alex says so.

Each policy has:

- `name`: shown next to every proposal it makes. Letters, digits, `-`, `_` and
  `.` only.
- `match`: the conditions a directory must meet. All of them must hold.
- `action`: `"propose"` (the default) only lists the directory in `stomp
  candidates`. `"apply"` also lets the daily run archive it.
- `wait`: for `apply` policies, how long a proposal waits before it is
  archived automatically, so you have time to see it and say no. The default is
  `"7d"`. The wait counts from when the directory was first proposed.
- `destination`: where the archive goes: `"local"` or a destination name. The
  default is `archive.default_destination`, which is `local` unless you set it.

{% note(kind="warning", title="The wait starts when a directory is first proposed") %}
If an `apply` policy has been proposing a directory for longer than its `wait`
by the time you turn on the daily run, the first run archives it. Look at
`stomp candidates` before you run `stomp schedule enable`.
{% end %}

An `apply` policy must include `grant_ended_for` or `unchanged_for`, so that
stomp only ever archives automatically data that nobody is working on.
Without one, stomp rejects the config with `action = "apply" needs
match.grant_ended_for or match.unchanged_for, so only data nobody is changing is
archived automatically`.

`stomp doctor` notices that one policy archives automatically but nothing runs
it yet:

```sh
stomp doctor
```

```text
…
info  schedule     daily runs are off; `stomp schedule enable` keeps usage, accounting, scans and proposals current
warn  policies     2 policies, 1 archiving automatically, but nothing runs them until `stomp schedule enable`

No failures; 1 warning(s) to look at.
```

## Know the conditions

| Condition | A directory matches when |
|-----------|--------------------------|
| `grant` | it belongs to this grant, or one of a list: `grant = ["NSF-2231", "EPSCOR-T2"]` |
| `path` | its path matches this pattern; `*` stays within one directory level, `**` crosses levels |
| `grant_ended_for` | its grant's `end` date is at least this long ago |
| `unchanged_for` | nothing in it has been modified or changed for at least this long |
| `min_bytes` | it holds at least this much data, such as `"100GiB"` |
| `min_files` | it holds at least this many files |
| `max_avg_file_size` | its files average at most this size, such as `"1MiB"` |

Durations are a number and a unit: `s`, `m` (minutes), `h`, `d`, `w` or `y`
(365 days). There is no unit for months; write `"90d"`. Sizes take `KiB`,
`MiB`, `GiB` and `TiB`, or `KB`, `MB`, `GB` and `TB` for powers of ten. A
`path` must be absolute or start with `~`.

`unchanged_for` is the condition for data that has simply gone quiet. stomp's
own suggestion for a second policy, which `stomp candidates` prints when you
have none, is:

```toml
[[policy]]
name  = "stale-small-files"
match = { unchanged_for = "1y", min_files = 10000, max_avg_file_size = "1MiB" }
```

That proposes directories of at least 10,000 small files that have sat
untouched for a year: the kind of directory where archiving saves the most
files. "Untouched" here is strict. Both the newest modification time and the
newest change time in the directory must be older than the period. Copying
data with `cp` or `rsync -a`, or restoring it, keeps the old modification times
but sets new change times, so data that has just arrived isn't proposed
straight away. That makes `unchanged_for` stricter than
[`stomp report stale`](@/reference/commands.md#stomp-report), which looks at
modification times alone.

## Know how stomp chooses directories

Policies are checked against your newest detailed scan, not the live
filesystem, so checking them is quick. The scan records every directory
except small ones (under 1 MiB and under 100 files, by default), which it
counts as part of their parent. stomp looks at each recorded directory and:

- **proposes the outermost match.** If `~/marsh_mapping` matches, its
  subdirectories aren't proposed separately, since archiving it covers them.
  When two policies match nested directories, the outer one wins; when they
  match the same directory, the policy that comes first in the config wins.
- **sticks to grant data.** Without a `path`, a policy only proposes
  directories inside your grants' directories, and nothing inside a hidden
  directory (one whose name starts with `.`). Software, settings and reference
  data are often read every day without ever being modified, so age alone
  mustn't put `~/bin`, `~/.conda` or a shared genome index up for archiving. A
  policy with a `path` can propose any directory its pattern matches, so
  write it carefully.
- **never proposes** a root, a directory that contains another grant's
  directory, an environment, a cache or anything else excluded from scans,
  stomp's own data and staging directories, or anything already archived,
  being archived, or gone since the scan.
- **leaves environments out of the sums.** A conda environment or virtual
  environment inside a proposed directory stays in place when it is archived,
  so it doesn't count toward the directory's size or file count, for the
  `min_bytes`, `min_files` and `max_avg_file_size` conditions as well.

## Run a scan

With no scan yet, there's nothing to check the policies against:

```sh
stomp candidates
```

```text
Policies are checked against a scan; run `stomp scan` (it runs as a Slurm task), then try again.
```

```sh
stomp scan
```

```text
Submitted scan task scan-20261118-180620-f9be as Slurm job 4810233.
Follow it with `stomp tasks`; once it's done, that (or any report) imports the results.
```

The [scans](@/concepts/scans.md) page explains what a scan records. Once the
task is done, the policies have something to work with.

## Review the proposals

```sh
stomp candidates
```

```text
stomp: estimating compression for 2 proposal(s)…
Archive proposals from the scan of 2026-11-18 18:06 EST, by 2 policies.

ID        SIZE     FILES        FREES   TO      GRANT       PATH                                  WHY
 1   913.9 GiB   457 → 6   ≈913.9 GiB   nas     USDA-1180   ~/marsh_mapping                       grant USDA-1180 ended 2026-06-30 (ended-grants) — archived automatically once `stomp schedule enable` is on
 2     2.4 MiB    36 → 7     ≈1.6 MiB   local   NSF-2231    ~/soil_carbon_runs/2024_calibration   matches ~/soil_carbon_runs/2024_* (finished-runs)

2 proposal(s), 913.9 GiB in 493 files (13 after archiving); archiving them frees about 913.9 GiB.
Archive with `stomp apply ID...` or `stomp apply --all`; skip one for good with `stomp candidates dismiss ID`.
Directories read but never modified (software, reference data) look unchanged too; check before archiving.
```

Each row is a proposal:

- **ID** is the proposal's number. It stays the same from run to run, so you
  can refer to it in `stomp apply` and `stomp candidates dismiss`.
- **SIZE** is what would be archived, leaving out environments and caches
  inside the directory, which stay in place.
- **FILES** shows the number of files now → the number left on the cluster
  afterwards. For a local archive that is the chunks plus six small records
  (`36 → 7`); for a remote one, just the six records (`457 → 6`).
- **FREES** is roughly how much space archiving would free on the cluster. For
  a remote destination it is the whole size: about 914 GiB for the marsh data.
  For a local archive it is the size minus the estimated compressed size: about
  1.6 MiB of the calibration runs' 2.4 MiB. A `?` means stomp hasn't estimated
  that proposal yet.
- **TO** is the destination: `local`, or the name of a remote destination.
- **GRANT** and **PATH** say whose data it is and where.
- **WHY** gives the reason, then the policy in parentheses. For an `apply`
  policy, it also says when the directory will be archived automatically.

The estimates are cheap. stomp compresses the first MiB of 16 files, picked
in proportion to their size, using the scan to know where to look. It
estimates up to 8 new proposals each time you run `stomp candidates`, and all
of them in the daily run, and estimates again when a proposal's size changes
by more than 25%. Before archiving a big directory, `stomp archive DIR
--estimate` gives a fuller estimate, as in
[Archive a finished project](@/tutorials/archive.md#estimate-the-result).

The last line of the summary is a reminder worth taking seriously: a policy
can't tell data nobody needs from data that is read every day but never
changed.

## Archive proposals with stomp apply

The calibration runs are ready to go. `stomp apply` takes one or more proposal
IDs, shows what it will do, and asks:

```sh
stomp apply 2
```

```text
These directories will be archived, each verified before its originals are removed:

ID      SIZE   FILES   TO      PATH
 2   2.4 MiB      36   local   ~/soil_carbon_runs/2024_calibration

Each runs as its own Slurm task.
Archive 1 directory? [y/N] y
2 ~/soil_carbon_runs/2024_calibration: archive NSF-2231-20261118-d0ca43 submitted as task archive-20261118-180622-b278 (Slurm job 4810234)
Follow them with `stomp tasks`.
```

Before anything is archived, each proposal is checked again, because the scan
may be days old. The policy must still be in the config, the directory must
still be one stomp may archive, and its grant and path conditions must still
hold under the current config. On CephFS, where these numbers are instant,
the directory's live size, file count and newest change must also still meet
the policy's size and age conditions, so a directory someone has written to
since the scan no longer passes `unchanged_for`. A proposal that fails is
skipped with a warning of the form `stomp: warning: skipping proposal ID
(PATH): REASON`, and the others go ahead.

Each archive is then an ordinary archive task, exactly like one started with
`stomp archive`. Other ways to run it:

- `stomp apply --all` takes every open proposal.
- `--here` archives in the foreground, one directory after another, instead of
  submitting tasks.
- `--yes` skips the question, and `--time`, `--cpus`, `--mem`, `--partition`
  and `--account` adjust the tasks.

Applied proposals drop off the list:

```sh
stomp candidates
```

```text
Archive proposals from the scan of 2026-11-18 18:06 EST, by 2 policies.

ID        SIZE     FILES        FREES   TO    GRANT       PATH              WHY
 1   913.9 GiB   457 → 6   ≈913.9 GiB   nas   USDA-1180   ~/marsh_mapping   grant USDA-1180 ended 2026-06-30 (ended-grants) — archived automatically once `stomp schedule enable` is on

1 proposal(s), 913.9 GiB in 457 files (6 after archiving); archiving them frees about 913.9 GiB.
Archive with `stomp apply ID...` or `stomp apply --all`; skip one for good with `stomp candidates dismiss ID`.
Directories read but never modified (software, reference data) look unchanged too; check before archiving.
```

## Turn on the daily run

Proposals only stay current if something checks the policies regularly, and
`apply` policies only act if something runs them. `stomp schedule enable` sets
up one small daily Slurm job to do both:

```sh
stomp schedule enable
```

```text
stomp will run daily at 03:32, through scrontab.
Each run records usage, imports job accounting, starts a scan once a week, and updates archive proposals, archiving those from "apply" policies once their wait has passed.
`stomp schedule status` shows how the runs went; logs are in ~/.local/share/stomp/schedule/logs.
```

Unless you give a time with `--at HH:MM`, stomp picks one between 02:00 and
05:00 that is different for each user, so that a whole lab's runs don't start
at once. Running `stomp schedule enable` again with another `--at` changes the
time.

stomp uses Slurm's own cron, `scrontab`, when your site allows it, and adds a
block like this to your scrontab, leaving any other entries alone:

```sh
scrontab -l
```

```text
# BEGIN stomp schedule (managed by `stomp schedule`; edits here are replaced)
#SCRON --job-name=stomp-schedule
#SCRON --output=/home/alex/.local/share/stomp/schedule/logs/run-%j.out
#SCRON --time=00:30:00
#SCRON --cpus-per-task=1
#SCRON --mem=2G
#SCRON --partition=standard
#SCRON --account=lab-rivera
32 3 * * * '/home/alex/bin/stomp' --config '/home/alex/.config/stomp/config.toml' schedule run
# END stomp schedule
```

The job is deliberately small: one CPU, 2 GB of memory and 30 minutes, using
the partition and account from your `[slurm]` settings. Anything heavy, such as
a scan or an archive, it submits as a task of its own.

Now `stomp doctor` is content, and the automatic proposal has a date:

```sh
stomp doctor
```

```text
…
ok    schedule     daily runs are on
ok    policies     2 policies, 1 archiving automatically
…
```

```sh
stomp candidates
```

```text
Archive proposals from the scan of 2026-11-18 18:06 EST, by 2 policies.

ID        SIZE     FILES        FREES   TO    GRANT       PATH              WHY
 1   913.9 GiB   457 → 6   ≈913.9 GiB   nas   USDA-1180   ~/marsh_mapping   grant USDA-1180 ended 2026-06-30 (ended-grants) — archived automatically after 2026-12-18
…
```

## Know what each run does

Every daily run:

1. imports finished scans, archive records and job logs;
2. records a usage sample, if all your roots are on CephFS, where that is
   instant (elsewhere it would mean walking every file, so it skips this);
3. imports job accounting from `sacct`;
4. submits a scan task if the last scan started about a week ago or more, or
   resumes one that stopped early, unless a scan is already queued or running;
5. checks the policies against the newest scan, updates the proposals, and
   estimates compression for new ones;
6. archives proposals from `apply` policies whose wait has passed: at most
   three a day, oldest first, each checked again first and each as its own
   archive task;
7. keeps the newest 30 run logs.

To see a run's work without waiting for the night, `stomp schedule run` does
it now, in the foreground:

```sh
stomp schedule run
```

```text
stomp scheduled run, 2026-11-18 18:06 EST
no usage sample: a root isn't on CephFS, so measuring means walking every file; `stomp usage` records one when you run it
imported 77 job record(s) from sacct
archive proposals: 1 open (0 new, 0 gone)
```

No scan was due, since the last one started minutes ago, and the automatic
proposal is still within its 30-day wait. `stomp schedule status` shows how the
schedule is set up and what the last run did:

```sh
stomp schedule status
```

```text
Runs daily at 03:32 through scrontab.

Last run: 2026-11-18 18:06 EST, ok
  no usage sample: a root isn't on CephFS, so measuring means walking every file; `stomp usage` records one when you run it
  imported 77 job record(s) from sacct
  archive proposals: 1 open (0 new, 0 gone)

Logs: ~/.local/share/stomp/schedule/logs
```

Each scheduled run writes its output to `run-<job ID>.out` in that logs
directory. `stomp schedule status` reports a problem, and exits with an error,
if the scrontab entry or queued job has disappeared or if no run has finished
for more than two days. `stomp doctor` warns when no run has finished for over
two days or the last run had problems.

## Dismiss a proposal

Alex's PI wants the marsh orthomosaics kept on the cluster until the follow-up
paper is out, so the automatic proposal has to go. `stomp candidates dismiss`
stops proposing a directory for good:

```sh
stomp candidates dismiss 1
```

```text
Dismissed 1 proposal(s); they won't be proposed again. `stomp candidates --all` lists them, and `stomp apply ID` still archives one.
```

A dismissed proposal is never archived automatically. With every match now
applied or dismissed, `stomp candidates` has nothing open to show:

```sh
stomp candidates
```

```text
Archive proposals from the scan of 2026-11-18 18:06 EST, by 2 policies.

No open proposals (1 applied, 1 dismissed); `stomp candidates --all` lists them.
```

`--all` lists past proposals too, with a **STATE** column:

```sh
stomp candidates --all
```

```text
Archive proposals from the scan of 2026-11-18 18:06 EST, by 2 policies.

ID   STATE                                     SIZE     FILES        FREES   TO      GRANT       PATH                                  WHY
 1   dismissed                            913.9 GiB   457 → 6   ≈913.9 GiB   nas     USDA-1180   ~/marsh_mapping                       grant USDA-1180 ended 2026-06-30 (ended-grants)
 2   applied (NSF-2231-20261118-d0ca43)     2.4 MiB    36 → 7     ≈1.6 MiB   local   NSF-2231    ~/soil_carbon_runs/2024_calibration   matches ~/soil_carbon_runs/2024_* (finished-runs)
```

An applied proposal shows the archive it became. When the marsh data can go,
`stomp apply 1` still archives the dismissed proposal.

## Schedule without scrontab

If your site doesn't allow `scrontab`, `stomp schedule enable` says so and
falls back to a Slurm job that queues the next one. You can also choose that
yourself with `--mode task`:

```sh
stomp schedule enable --mode task --at 03:30
```

```text
stomp will run daily at 03:30, as a Slurm job that queues the next one; the first is job 4810235, starting 2026-11-19 03:30.
Each run records usage, imports job accounting, starts a scan once a week, and updates archive proposals, archiving those from "apply" policies once their wait has passed.
`stomp schedule status` shows how the runs went; logs are in ~/.local/share/stomp/schedule/logs.
```

Switching modes removes the scrontab entry. The job waits in the queue until
its start time. Each run first submits the next day's job and then does its
work, so one failed run doesn't break the chain:

```sh
stomp schedule status
```

```text
Runs daily at 03:30 as a Slurm job that queues the next one.
  Next run: Slurm job 4810235 (pending)

Last run: 2026-11-18 18:06 EST, ok
…
```

If the queued job ever disappears, for example because someone cancelled it,
`stomp schedule status` says so; run `stomp schedule enable` again to restart
the chain.

## Turn it off

```sh
stomp schedule disable
```

```text
Recurring runs are off (removed queued Slurm job 4810235).
```

This removes the scrontab entry or cancels the queued job, whichever is in
use. Your policies stay in the config, and `stomp candidates` and `stomp
apply` keep working by hand.

## What's next

- [Policies](@/concepts/policies.md) explains the rules behind proposals in
  more depth.
- The [`[[policy]]` reference](@/reference/configuration.md#policy) lists
  every key.
- [Close out a grant](@/tutorials/grant-closeout.md) walks through what to
  do when a grant ends.
- [Send archives to a NAS or Spaces](@/tutorials/nas-and-spaces.md) sets up
  the destination the `ended-grants` policy uses.
