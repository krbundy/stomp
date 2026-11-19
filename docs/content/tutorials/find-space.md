+++
title = "Find what's using your space"
description = "Directory sizes, a detailed scan, stale data, caches and environments, and what changed."
weight = 20

[extra]
time = "25 minutes"
needs = "a config with your grants and [slurm] settings (see the first tutorial)"
+++

You'll find out where Alex's 1.7 TiB goes: directory sizes with `stomp du`, a
detailed scan that runs as a Slurm task, reports of data nobody has touched and
of caches and software environments, and what changed since you last looked.
At the end you'll have a short list of directories worth cleaning up or
archiving.

## Look at directory sizes

`stomp du` is the quick look. It shows a directory and its subdirectories,
largest first, with the grant each one counts toward:

```sh
stomp du ~
```

```text
stomp: ~ is not on CephFS, so stomp has to walk it to measure sizes. This can take a while.
     SIZE   FILES   LAST CHANGE   GRANT                                     PATH
  1.7 TiB   1,379   just now      contains NSF-2231, USDA-1180, EPSCOR-T2   ~
913.9 GiB     457   just now      USDA-1180                                   marsh_mapping/
509.6 GiB      66   just now      NSF-2231                                    soil_carbon_runs/
179.2 GiB      46   just now      EPSCOR-T2                                   forest_plots/
 82.8 GiB     100   just now      NSF-2231                                    soil_carbon/
 59.0 GiB       2   just now      —                                           scratch/
  7.3 GiB     501   just now      —                                           miniconda3/
  5.0 GiB      82   just now      —                                           containers/
  3.5 GiB     120   just now      —                                           .cache/
117.2 KiB       3   just now      —                                           papers/
    868 B       1   just now      —                                           .config/
     28 B       1                 —                                           (files here)
```

These examples come from a test machine without CephFS, so stomp walks the
tree and says so on the first line. On the cluster's CephFS, `stomp du` reads
the totals CephFS keeps for every directory, so it's instant however many
files there are, and that line doesn't appear. Later outputs on this page
leave out such notes.

Without a path, `stomp du` shows the current directory. The columns are the
ones `stomp usage` uses (see
[the first tutorial](@/tutorials/first-setup.md#see-which-grant-each-directory-belongs-to)):
size, file count, how long ago anything in the directory last changed, and
its grant.

To see one more level, use `-d` (`--depth`):

```sh
stomp du ~ -d 2
```

```text
     SIZE   FILES   LAST CHANGE   GRANT                                     PATH
  1.7 TiB   1,379   just now      contains NSF-2231, USDA-1180, EPSCOR-T2   ~
913.9 GiB     457   just now      USDA-1180                                   marsh_mapping/
913.9 GiB      40   just now      USDA-1180                                     orthomosaics/
 25.7 MiB      16   just now      USDA-1180                                     classified_tiles/
800.0 KiB     400   just now      USDA-1180                                     field_photos/
     16 B       1                 USDA-1180                                     (files here)
509.6 GiB      66   just now      NSF-2231                                    soil_carbon_runs/
509.6 GiB      30   just now      NSF-2231                                      2025_scenarios/
  2.4 MiB      36   just now      NSF-2231                                      2024_calibration/
179.2 GiB      46   just now      EPSCOR-T2                                   forest_plots/
179.2 GiB      16   just now      EPSCOR-T2                                     lidar/
735.5 KiB      30   just now      EPSCOR-T2                                     inventory/
…
 59.0 GiB       2   just now      —                                           scratch/
  7.3 GiB     501   just now      —                                           miniconda3/
  4.7 GiB     200   just now      —                                             pkgs/
  2.6 GiB     301   just now      —                                             envs/
…
```

Three things stand out: 914 GiB of orthomosaics for `USDA-1180`, a grant that
has ended; 510 GiB of 2025 model scenarios; and 59 GiB in `~/scratch` that no
grant pays for. `stomp du` can't say how old the data is or what kind of files
it holds. For that you need a scan.

## Run a detailed scan

A scan walks your roots and records, for each directory, its size and file
count, how old its files are, which file types take the space, and where the
caches and software environments are. Reports read the newest scan, so they
don't walk anything themselves.

A scan looks at every file's details, which can take hours for millions of
files, so it runs as a Slurm task by default, using your `[slurm]` settings:

```sh
stomp scan
```

```text
Submitted scan task scan-20261118-180415-f2a6 as Slurm job 4810233.
Follow it with `stomp tasks`; once it's done, that (or any report) imports the results.
```

`stomp tasks` lists stomp's tasks and their state. Once the job has finished:

```sh
stomp tasks
```

```text
TASK                        STATUS   SLURM JOB   UPDATED            PROGRESS
scan-20261118-180415-f2a6   done     4810233     2026-11-18 18:04   1,386 files, 1.7 TiB
```

STATUS comes from Slurm while the job is queued or running (`pending`,
`running`), and from the task itself once it's over (`done`, `failed`,
`cancelled`, or `incomplete` for a scan that stopped early). A status ending
in "(not in queue; see its log)" means the job ended without reporting back,
and its log says why. PROGRESS is updated every 10 seconds while the task
runs. For the details of one task:

```sh
stomp tasks show scan-20261118-180415-f2a6
```

```text
Task      scan-20261118-180415-f2a6
Kind      scan
Status    done
Created   2026-11-18 18:04
Updated   2026-11-18 18:04
Jobs      4810233
Progress  1,386 files, 1.7 TiB
Message   full scan, 68 directories and 1,386 files (1.7 TiB) in 0s
Script    ~/.local/share/stomp/tasks/scan-20261118-180415-f2a6/task.sh
Log       ~/.local/share/stomp/tasks/scan-20261118-180415-f2a6/slurm-4810233.out
```

`Jobs` lists every Slurm job the task has run as, `Script` is the batch script
stomp generated, and `Log` is Slurm's output, the place to look if a task
failed. A scan of Alex's few thousand files takes no time; yours will take
longer. To stop a task, run `stomp tasks cancel ID`.

The first scan walks everything. On CephFS, later scans skip directories
nothing has changed in since the previous scan, and reuse what it found, so
they're much faster; every 30 days (`scan.full_every`), or when you run
`stomp scan --full`, a scan walks everything again. On other filesystems every
scan walks everything.

### The results import themselves

A task never writes stomp's catalog directly; it leaves its results in an
inbox. The `stomp tasks` you just ran imported the finished scan, and so would
any report, `stomp usage` or `stomp changes`, without saying anything.
`stomp ingest` imports explicitly and tells you what the catalog holds:

```sh
stomp ingest
```

```text
The catalog has 0 measurement(s), 1 scan(s), 0 job record(s) and 0 archive(s).
```

`stomp du` doesn't read the catalog, so it doesn't import, and `stomp doctor`
only reports finished scans that are still waiting.

### Scan on the login node with --here

If your roots are small, you can run the scan in the foreground instead:

```sh
stomp scan --here
```

```text
stomp: scanning here instead of in a Slurm task. This walks every root in your config; on a shared login node, keep that to small ones.
Scan complete: full scan, 68 directories and 1,387 files (1.7 TiB) in 0s.
Imported into the catalog.
```

The results are imported at once. As the warning says, `stomp scan` has no
path argument: it always scans every root in your config, so `--here` is only
for when all your roots are small. Don't use it on a home directory with
millions of files. Login nodes are shared, every file the scan looks at is a
request to the file servers everyone uses, and a scan that runs for hours on a
login node is likely to be stopped. If you press Ctrl-C, the scan stops
cleanly, keeps what it finished, and `stomp scan --resume --here` continues
it.

### When a scan reaches its time limit

Scan tasks ask Slurm for a warning five minutes before their time limit. When
the warning comes, the scan stops starting new work and keeps what it has
finished: the directories two levels below each root are saved one by one as
they're done. The task then submits itself again, and the new job continues
where the old one stopped. Its log says "Scan stopped early after …; the
finished parts are saved. Resubmitted as Slurm job … to continue." and
`stomp tasks show` lists every job it has run as and how many times it was
resubmitted.

A task resubmits itself at most 5 times. After that, or if you set
`auto_resubmit = false` in `[slurm]`, its status becomes `incomplete` and
`stomp scan --resume` continues it. An interrupted scan that's never resumed
is discarded after 14 days. A scan that was interrupted and resumed records
everything except the file types and largest files, so reports that show
those use the newest scan that ran without interruption. If your scans keep
needing resubmits, give them more time with `time` in `[slurm]` or
`stomp scan --time 24:00:00`.

## Find data nobody has touched

`stomp report stale` lists the largest directories in which no file has been
modified in 180 days:

```sh
stomp report stale
```

```text
Directories with no file modified since 2026-05-22, from the scan of 2026-11-18 18:04 EST.

     SIZE   FILES   NEWEST FILE   GRANT       PATH
 25.7 MiB      16   200d ago      USDA-1180   ~/marsh_mapping/classified_tiles
  2.4 MiB      36   380d ago      NSF-2231    ~/soil_carbon_runs/2024_calibration
  1.4 MiB      12   200d ago      NSF-2231    ~/soil_carbon/data/raw
800.0 KiB     400   500d ago      USDA-1180   ~/marsh_mapping/field_photos
```

NEWEST FILE is how long ago the most recently modified file in the directory
was modified. Only the topmost stale directory of each subtree is listed,
because everything inside it is stale too. Roots, caches, environments,
directories in `scan.exclude` and archives are never listed. The report shows
the 20 largest (`--top` changes that); when there are more, a last line gives
the total size of all of them.

stomp doesn't read the dates of files inside caches and environments (see
[below](#find-caches-and-environments)). Instead, the last time anything in
one changed counts as activity in the directories that contain it. So a
directory holding an environment that was updated recently isn't listed, even
if nothing else in it has been modified for years.

Read the list with what you know about the data. Here,
`~/soil_carbon_runs/2024_calibration` hasn't changed in over a year: a
finished set of runs, and a good candidate for archiving. The raw field data
in `~/soil_carbon/data/raw` is old too, but `NSF-2231` is still running, so
Alex keeps it where it is.

`--older-than` takes an age (`90d`, `1y`) or a date (`2026-01-01`):

```sh
stomp report stale --older-than 1y
```

```text
Directories with no file modified since 2025-11-18, from the scan of 2026-11-18 18:04 EST.

     SIZE   FILES   NEWEST FILE   GRANT       PATH
  2.4 MiB      36   380d ago      NSF-2231    ~/soil_carbon_runs/2024_calibration
800.0 KiB     400   500d ago      USDA-1180   ~/marsh_mapping/field_photos
```

`--grant` limits the list to one grant. `USDA-1180` ended on 2026-06-30, so
ask what in it hasn't changed for 90 days:

```sh
stomp report stale --grant USDA-1180 --older-than 90d
```

```text
Directories with no file modified since 2026-08-20, from the scan of 2026-11-18 18:04 EST.

     SIZE   FILES   NEWEST FILE   GRANT       PATH
913.9 GiB     457   160d ago      USDA-1180   ~/marsh_mapping
```

The whole directory: nothing in `~/marsh_mapping` has been modified since
before the grant ended. That's 914 GiB to archive, which the
[grant closeout tutorial](@/tutorials/grant-closeout.md) does.

## Find caches and environments

Some directories stomp treats specially. **Caches** are known cache locations:
`~/.cache`, `~/.apptainer/cache`, `~/.singularity/cache`, `~/.conda/pkgs`, and
the `pkgs` directory of a conda install in your home (`~/miniconda3`,
`~/anaconda3`, `~/miniforge3` or `~/mambaforge`). **Environments** are
software installs stomp recognizes by what's in them: an Apptainer sandbox (a
`.singularity.d` directory), a conda environment (`conda-meta`) or a Python
venv (`pyvenv.cfg`). Both kinds count toward the grant whose directory they're
in, at full size, but scans don't look into them, and stomp never archives
them: they hold many small files that are easier to rebuild or clean than to
archive.

`stomp report reclaimable` lists them, with what you can do about each:

```sh
stomp report reclaimable
```

```text
Caches and software environments, from the scan of 2026-11-18 18:04 EST.

KIND              SIZE   FILES   GRANT      PATH                        SUGGESTION
cache          4.7 GiB     200   —          ~/miniconda3/pkgs           conda's package cache: `conda clean --all`
cache          3.5 GiB     120   —          ~/.cache                    usually safe to clear; check what's in it first (pip: pip cache purge)
environment    3.1 GiB      81   —          ~/containers/qgis_sandbox   Apptainer sandbox: `apptainer build image.sif <dir>` makes it one file
environment    2.6 GiB     301   —          ~/miniconda3/envs/geo       conda environment: can be rebuilt from an environment.yml
environment   70.3 KiB      61   NSF-2231   ~/soil_carbon/.venv         Python venv: can be rebuilt from requirements
----------------------------------------------------------------------------------------------------------------------------------------------
cache          8.2 GiB                      total in caches
```

- **`~/miniconda3/pkgs`** is where conda keeps the packages it downloaded.
  `conda clean --all` removes the downloaded archives and the packages no
  environment uses; your environments keep working.
- **`~/.cache`** holds pip's download cache (`~/.cache/pip`, all 3.5 GiB of
  it). `pip cache purge` empties it, and pip downloads again whatever it needs
  later.
- **The QGIS Apptainer sandbox** is 81 files here, but a real one often holds
  tens of thousands. `apptainer build` packs it into a single `.sif` file,
  which is kinder to the shared file servers and to any limit on your number
  of files.
- **The venv in `~/soil_carbon`** counts toward `NSF-2231` because it's inside
  that grant's directory.

If you have another directory that should be treated like this, list it
under `scan.exclude` in your config; after the next scan it appears here with
KIND `user`.

## See what changed for each grant

`stomp changes` compares your storage now with an earlier measurement. Every
`stomp usage` run records one. If there isn't one yet, it says so:

```sh
stomp changes
```

```text
stomp: error: nothing to compare with yet; run `stomp usage` to record a measurement
```

So record one:

```sh
stomp usage
```

```text
Measured 2026-11-18 18:04 EST

ROOT   FILESYSTEM      SIZE   FILES   QUOTA
~      POSIX        1.7 TiB   1,385   —

GRANT          DIRS        SIZE   FILES   LAST CHANGE
USDA-1180         1   913.9 GiB     457   just now
NSF-2231          2   592.5 GiB     166   just now
EPSCOR-T2         1   179.2 GiB      46   just now
(unassigned)      —    74.9 GiB     716   just now
-----------------------------------------------------
TOTAL                   1.7 TiB   1,385   just now
```

Now some work happens. A model run writes the first 2026 scenario, 15 GiB in
`~/soil_carbon_runs/2026_scenarios`, and Alex deletes an old test file from
`~/scratch`:

```sh
rm ~/scratch/old_test.nc
```

Ask what changed:

```sh
stomp changes
```

```text
Changes since 2026-11-18 18:04 EST (a recorded measurement) to now.

GRANT               THEN         NOW      CHANGE   FILES CHANGE   LAST CHANGE
USDA-1180      913.9 GiB   913.9 GiB         0 B              0   2m ago
NSF-2231       592.5 GiB   607.5 GiB   +15.0 GiB             +2   just now
EPSCOR-T2      179.2 GiB   179.2 GiB         0 B              0   2m ago
(unassigned)    74.9 GiB    63.9 GiB   -11.0 GiB             -1   just now
-----------------------------------------------------------------------------
TOTAL            1.7 TiB     1.7 TiB    +4.0 GiB             +1   just now
```

Each row compares a grant's size in the recorded measurement (THEN) with a
fresh one (NOW). CHANGE and FILES CHANGE are the differences, and LAST CHANGE
is how long ago anything in the grant's directories last changed. `NSF-2231`
grew by the new scenario, and the deleted file shows up in `(unassigned)`,
because `~/scratch` belongs to no grant. On CephFS the fresh measurement is
instant, so `stomp changes` is a cheap way to check whether anything is
growing.

`--grant` shows one grant:

```sh
stomp changes --grant NSF-2231
```

```text
Changes since 2026-11-18 18:04 EST (a recorded measurement) to now.

GRANT           THEN         NOW      CHANGE   FILES CHANGE   LAST CHANGE
NSF-2231   592.5 GiB   607.5 GiB   +15.0 GiB             +2   just now
```

`stomp changes` doesn't record its own measurement, so it keeps comparing with
the same one until you run `stomp usage` again. That makes the default "since
I last ran `stomp usage`". Once you've run `stomp usage`, the changes before it
are part of the baseline; to look further back, use `--since` with an age
(`--since 30d`) or a date (`--since 2026-10-01`), and stomp compares with the
newest measurement at or before then. `stomp report history` lists the
measurements you have. On CephFS, the
[daily run](@/tutorials/automate.md) records one every day, which gives
`--since` a measurement for every date after you turn it on.

## See which directories changed

`stomp changes` says that a grant grew, not where. For that, compare two
scans: `stomp changes --dirs` compares the newest scan with the one before it.
Alex's last scan was before the new model run, so start another:

```sh
stomp scan
```

```text
Submitted scan task scan-20261118-180647-17ea as Slurm job 4810234.
Follow it with `stomp tasks`; once it's done, that (or any report) imports the results.
```

When the task has finished:

```sh
stomp changes --dirs
```

```text
Directory changes between the scans of 2026-11-18 18:04 EST and 2026-11-18 18:06 EST, down to depth 2.
Small directories are folded into their parents, so they show up there.

    CHANGE         NOW      BEFORE   FILES CHANGE   STATUS    PATH
 +15.0 GiB   524.6 GiB   509.6 GiB             +2   changed   ~/soil_carbon_runs
 +15.0 GiB    15.0 GiB         0 B             +2   new       ~/soil_carbon_runs/2026_scenarios
 -11.0 GiB    48.0 GiB    59.0 GiB             -1   changed   ~/scratch
  +4.0 GiB     1.7 TiB     1.7 TiB             +5   changed   ~
+105.3 KiB   331.0 KiB   225.8 KiB             +4   changed   ~/.local
+105.3 KiB   331.0 KiB   225.8 KiB             +4   changed   ~/.local/share
```

Rows are sorted by the size of the change, largest first, down to two levels
below each root (`--depth` changes that) and at most 20 of them (`--top`).
STATUS is `new` for a directory that wasn't in the earlier scan, `gone` for
one that isn't in the newer scan, and `changed` otherwise. Every change also
shows in the directories above it, which is why `~` and
`~/soil_carbon_runs` are listed too. The small change in `~/.local/share` is
stomp itself: its catalog and task files live in `~/.local/share/stomp` and
grow a little with each scan.

Directories under 1 MiB with fewer than 100 files aren't recorded on their own
(their numbers are added to their parent's), so small changes show up one
level higher.

With `--grant`, the depth counts from each of the grant's directories:

```sh
stomp changes --dirs --grant NSF-2231 --depth 3
```

```text
Directory changes between the scans of 2026-11-18 18:04 EST and 2026-11-18 18:06 EST, down to depth 3.
Small directories are folded into their parents, so they show up there.

   CHANGE         NOW      BEFORE   FILES CHANGE   STATUS    PATH
+15.0 GiB   524.6 GiB   509.6 GiB             +2   changed   ~/soil_carbon_runs
+15.0 GiB    15.0 GiB         0 B             +2   new       ~/soil_carbon_runs/2026_scenarios
+15.0 GiB    15.0 GiB         0 B             +2   new       ~/soil_carbon_runs/2026_scenarios/scenario_00
```

`--since` works here too, and picks the newest scan at or before that time
as the earlier one.

## Explore interactively

The reports answer specific questions. To wander around instead, `stomp tui`
opens an ncdu-style browser over your roots, with sizes, grants, and marks for
environments, caches and archives, and it can show a directory's history
from past scans. [Browse your storage with stomp tui](@/tutorials/browse.md)
shows how.

## What's next

- [Archive a finished project](@/tutorials/archive.md), such as
  `~/soil_carbon_runs/2024_calibration`.
- [Close out a grant that has ended](@/tutorials/grant-closeout.md), like
  `USDA-1180`.
- [Scans and the catalog](@/concepts/scans.md) explains what a scan records
  and how incremental scans work.
- The [command reference](@/reference/commands.md#stomp-report) lists every
  report option.
