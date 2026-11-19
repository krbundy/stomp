+++
title = "Attribute compute jobs to grants"
description = "CPU and GPU hours per grant from Slurm accounting, with one line in a job script when needed."
weight = 40

[extra]
time = "20 minutes"
needs = "a config with your grants (see the first tutorial) and a cluster where sacct works"
+++

Slurm charges your jobs to an account such as `lab-rivera`, not to a grant, so
the cluster can't tell you how many CPU-hours each grant used. In this tutorial
you'll see how stomp attributes Alex's jobs, place the ones it can't, add one
line to a job script so future jobs are logged, and finish with a compute
report per grant.

## See your recent jobs and their grants

```sh
stomp jobs list
```

```text
JOB       NAME          STATE       ENDED              CPU-H   GPU-H   GRANT          BY
4796924   century       COMPLETED   2026-10-21 23:16     351     0.0   NSF-2231       workdir
4805171   jupyter       COMPLETED   2026-10-25 06:46      19     0.0   (unassigned)   —
4804937   jupyter       COMPLETED   2026-10-25 15:38      19     0.0   (unassigned)   —
4791964   soc_fit       COMPLETED   2026-10-26 10:13      16     0.0   NSF-2231       workdir
4795947   century       COMPLETED   2026-10-26 20:11     425     0.0   NSF-2231       workdir
4806479   merge_nc      COMPLETED   2026-10-29 21:24     4.3     0.0   (unassigned)   —
…
4799270   marsh_unet    COMPLETED   2026-11-13 10:32      31     7.7   USDA-1180      workdir
4794845   century       COMPLETED   2026-11-13 20:12     400     0.0   NSF-2231       workdir
4805485   jupyter       COMPLETED   2026-11-14 20:38      17     0.0   (unassigned)   —
4792876   soc_fit       COMPLETED   2026-11-15 15:41      24     0.0   NSF-2231       workdir
4802698   lidar_tiles   CANCELLED   2026-11-16 05:00      41     0.0   EPSCOR-T2      workdir
4795641   century       COMPLETED   2026-11-18 06:20     419     0.0   NSF-2231       workdir
```

stomp gets these records from Slurm accounting with `sacct`. You didn't have to
import anything: the first time you run a command that needs job records, stomp
imports the last 90 days of your jobs, and after that each such command imports
what's new if the last import is more than 15 minutes old. The records are
copied into stomp's catalog, so your reports keep working after the site purges
old accounting.

The columns:

- **JOB** is the Slurm job ID. Tasks of a job array appear as `4810240_1`,
  `4810240_2` and so on.
- **STATE** is how the job ended: `COMPLETED`, `FAILED`, `TIMEOUT`,
  `CANCELLED` and so on.
- **ENDED** is when it finished, in local time, or `running`.
- **CPU-H** is the CPUs allocated to the job times how long it ran (sacct's
  `CPUTimeRAW`). It's what you were allocated, not what your program kept
  busy: a 32-CPU job that ran for 11 hours counts 352 CPU-hours.
- **GPU-H** is the GPUs allocated (`gres/gpu` in the job's `AllocTRES`) times
  how long it ran.
- **GRANT** is the grant the job is attributed to, or `(unassigned)`.
- **BY** is how stomp decided: `workdir`, `log`, `task`, or `—` when nothing
  matched. The next step explains each.

The list covers jobs that ended in the last 30 days, plus any still running.
`--since` takes an age such as `7d` or `2w`, or a date such as `2026-09-01`. It
only searches what stomp has already imported; it doesn't fetch older jobs
from Slurm.

## Understand how a job gets its grant

stomp checks these in order and stops at the first that applies:

1. **A tag on the job.** A tag comes from `stomp jobs log` run inside the job
   script, from `stomp jobs log --job` or `stomp jobs retag` run by hand
   afterwards, or from stomp itself for its own Slurm tasks. `BY` shows `log`
   for the first three and `task` for stomp's own.
2. **A tag on the job's array.** A task such as `4810240_7` with no tag of its
   own uses the tag of `4810240`, which is how one log line covers a whole job
   array.
3. **The working directory** (`BY` is `workdir`). If the directory the job ran
   in, as sacct recorded it, is inside one of a grant's directories, the job
   belongs to that grant. This is the directory you ran `sbatch` from, or the
   one you gave with `--chdir`.
4. **Nothing matched**, and the job is `(unassigned)`.

A tag always beats the working directory, even when the two disagree. Each job
has at most one tag. A tag set by hand replaces any earlier tag, and a tag
logged from inside a job never replaces one set by hand, so a requeued job
can't undo your fix.

Most of Alex's jobs need nothing: the `century` model runs from
`~/soil_carbon_runs/2025_scenarios`, which belongs to `NSF-2231`, and the
`lidar_tiles` jobs run from `~/forest_plots/lidar` (`EPSCOR-T2`). Attribution
by working directory follows your config: if you move a directory to another
grant, the jobs inferred from it move too. Tags never change on their own.

## Find the jobs with no grant

```sh
stomp jobs list --unassigned
```

```text
JOB       NAME       STATE       ENDED              CPU-H   GPU-H   GRANT          BY
4805171   jupyter    COMPLETED   2026-10-25 06:46      19     0.0   (unassigned)   —
4804937   jupyter    COMPLETED   2026-10-25 15:38      19     0.0   (unassigned)   —
4806479   merge_nc   COMPLETED   2026-10-29 21:24     4.3     0.0   (unassigned)   —
4807325   merge_nc   COMPLETED   2026-11-06 00:31     4.7     0.0   (unassigned)   —
4807442   merge_nc   COMPLETED   2026-11-11 08:25     5.0     0.0   (unassigned)   —
4805485   jupyter    COMPLETED   2026-11-14 20:38      17     0.0   (unassigned)   —
```

Two kinds of job are left over. The `jupyter` sessions start in Alex's home
directory, and `merge_nc` runs from `~/scratch`. Neither directory belongs to a
grant, so the working directory says nothing. The `merge_nc` jobs combine the
`century` output, so they belong to `NSF-2231`.

To look at one grant instead, use `--grant`. Here are the `USDA-1180` jobs
since September:

```sh
stomp jobs list --grant USDA-1180 --since 2026-09-01
```

```text
JOB       NAME         STATE       ENDED              CPU-H   GPU-H   GRANT       BY
4799140   marsh_unet   COMPLETED   2026-09-23 12:11      27     6.7   USDA-1180   workdir
4799033   marsh_unet   COMPLETED   2026-10-03 12:24      35     8.7   USDA-1180   workdir
4799270   marsh_unet   COMPLETED   2026-11-13 10:32      31     7.7   USDA-1180   workdir
```

`USDA-1180` ended in June, yet three jobs ran from `~/marsh_mapping` since. The
working directory still points there, so they count toward the ended grant.
[Close out a grant that has ended](@/tutorials/grant-closeout.md) deals with
jobs like these.

## Log jobs from the job script

For jobs that don't run from a grant directory, add `stomp jobs log` with the
grant ID to the job script. This is `~/scratch/merge_nc.sbatch` with the line
added:

```sh
#!/bin/bash
#SBATCH --job-name=merge_nc
#SBATCH --account=lab-rivera
#SBATCH --partition=standard
#SBATCH --cpus-per-task=4
#SBATCH --time=02:00:00
#SBATCH --output=merge_nc_%j.out

stomp jobs log NSF-2231 --note "merge 2025 scenario output"

module load nco
srun ncrcat ~/soil_carbon_runs/2025_scenarios/scenario_*/state.nc merged.nc
```

Put the line before the real work, so the job is logged even if it later fails
or runs out of time. Submit the job as usual:

```sh
cd ~/scratch
sbatch merge_nc.sbatch
```

```text
Submitted batch job 4810233
```

stomp's message goes to the job's output file:

```sh
head -1 merge_nc_4810233.out
```

```text
stomp: logged job 4810233 to NSF-2231
```

`stomp jobs log` reads the job ID from `SLURM_JOB_ID` and writes one small file
to `~/.local/share/stomp/inbox/jobs/`. It doesn't open stomp's catalog or wait
for any lock, so it returns at once even when hundreds of jobs start together.
The next stomp command you run imports the record, and once the job appears in
Slurm accounting, `stomp jobs list` shows it with `log` in the `BY` column.

The job needs `stomp` on its `PATH`. Batch jobs normally inherit the
environment you submit from; if yours don't, write the full path, such as
`~/bin/stomp jobs log NSF-2231`.

`--note` stores a short text with the record, for example which experiment the
job belongs to. `stomp jobs list` shows it in a NOTE column, as you'll see
below, and its CSV and JSON output have a `note` field.

### Job arrays

In a job array every task runs the line, but stomp logs the array once, under
its array job ID (`SLURM_ARRAY_JOB_ID`). The first task to get there writes the
record and prints:

```text
stomp: logged job array 4810240 to NSF-2231
```

Every other task finds the record and does nothing more:

```text
stomp: job array 4810240 is already logged
```

All the array's tasks are then attributed through that one tag.

### It never makes a job fail

A job that ran but wasn't logged is a small problem; a job that died because
of a logging line would be a big one. So `stomp jobs log` prints problems as
warnings and exits with status 0, whatever goes wrong. Run outside a job, for
example, it has nothing to log:

```sh
stomp jobs log NSF-2231; echo "exit status $?"
```

```text
stomp: warning: job not logged: not running inside a Slurm job (SLURM_JOB_ID is not set); use --job <ID> to tag a job that already ran
exit status 0
```

The same goes for a mistyped option, a config file it can't read, and a grant
ID that isn't in your config. A job with an unknown grant ID is still logged,
and shows up as unknown until you fix it (see
[Fix a wrong grant](#fix-a-wrong-grant) below).

If you'd rather have the job stop than run unlogged, for example in a script
that uses `set -e`, add `--strict`. Problems are then errors, with exit
status 1:

```sh
stomp jobs log NSF-2231 --strict; echo "exit status $?"
```

```text
stomp: error: job not logged: not running inside a Slurm job (SLURM_JOB_ID is not set); use --job <ID> to tag a job that already ran
exit status 1
```

With `--strict`, a grant ID that isn't in your config is an error too, and
nothing is recorded. Inside a job, `stomp jobs log NSF2231 --strict` prints:

```text
stomp: error: grant "NSF2231" is not in ~/.config/stomp/config.toml; the job was not logged
```

A mistyped option gives exit status 2.

## Tag jobs that already ran

The new line only helps future jobs. To tag the three `merge_nc` jobs that
already ran, give their IDs with `--job`:

```sh
stomp jobs log NSF-2231 --job 4806479 --job 4807325 --job 4807442 --note "merge scenario output"
```

```text
stomp: logged job 4806479 to NSF-2231
stomp: logged job 4807325 to NSF-2231
stomp: logged job 4807442 to NSF-2231
```

Tags set this way count as set by hand, so they replace any tag the jobs
already had. The jobs now belong to `NSF-2231`, with `log` in the BY column and
the note beside them:

```sh
stomp jobs list --grant NSF-2231 --since 2026-10-28
```

```text
JOB       NAME       STATE       ENDED              CPU-H   GPU-H   GRANT      BY        NOTE
4806479   merge_nc   COMPLETED   2026-10-29 21:24     4.3     0.0   NSF-2231   log       merge scenario output
4797638   century    COMPLETED   2026-10-30 15:07     229     0.0   NSF-2231   workdir
4798369   century    COMPLETED   2026-10-31 15:21     374     0.0   NSF-2231   workdir
4797924   century    COMPLETED   2026-11-03 18:46     444     0.0   NSF-2231   workdir
4795134   century    COMPLETED   2026-11-04 05:28     319     0.0   NSF-2231   workdir
4807325   merge_nc   COMPLETED   2026-11-06 00:31     4.7     0.0   NSF-2231   log       merge scenario output
…
```

The NOTE column only appears when at least one of the listed jobs has a note.
Only the notebook sessions are left without a grant:

```sh
stomp jobs list --unassigned
```

```text
JOB       NAME      STATE       ENDED              CPU-H   GPU-H   GRANT          BY
4805171   jupyter   COMPLETED   2026-10-25 06:46      19     0.0   (unassigned)   —
4804937   jupyter   COMPLETED   2026-10-25 15:38      19     0.0   (unassigned)   —
4805485   jupyter   COMPLETED   2026-11-14 20:38      17     0.0   (unassigned)   —
```

If you remember which grant a session was for, tag it the same way. Otherwise
it's fine to leave it: `(unassigned)` in a report is more honest than a guess.

## Fix a wrong grant

Mistakes happen in job scripts too. Suppose the `century` script had said
`stomp jobs log NSF2231`, without the hyphen. The job still ran, and its output
file held this warning:

```text
stomp: logged job 4795641 to NSF2231
stomp: warning: grant "NSF2231" is not in ~/.config/stomp/config.toml; the job is logged anyway and will show as an unknown grant until you fix it
```

That job ran inside `~/soil_carbon_runs`, but a tag beats the working
directory, so the list now shows the typo:

```sh
stomp jobs list --since 2d
```

```text
JOB       NAME      STATE       ENDED              CPU-H   GPU-H   GRANT               BY
4795641   century   COMPLETED   2026-11-18 06:20     419     0.0   NSF2231 (unknown)   log
```

`stomp jobs retag` assigns jobs to a grant by hand, replacing whatever tag they
had. It takes a grant from your config and one or more job IDs:

```sh
stomp jobs retag NSF-2231 4795641
```

```text
Tagged 1 job(s) as NSF-2231.
```

```sh
stomp jobs list --since 2d
```

```text
JOB       NAME      STATE       ENDED              CPU-H   GPU-H   GRANT      BY
4795641   century   COMPLETED   2026-11-18 06:20     419     0.0   NSF-2231   log
```

Since this job ran inside the grant's directory anyway, removing its tag would
have worked too. `stomp jobs untag` removes a job's tag, whoever set it, and
the job goes back to being attributed by its working directory:

```sh
stomp jobs untag 4795641
```

```text
Removed 1 tag(s). Those jobs are now attributed by their working directory, if it's in a grant.
```

```sh
stomp jobs list --since 2d
```

```text
JOB       NAME      STATE       ENDED              CPU-H   GPU-H   GRANT      BY
4795641   century   COMPLETED   2026-11-18 06:20     419     0.0   NSF-2231   workdir
```

Then fix the typo in the script, so the next run logs the right grant.

## Import accounting now

Commands that need job records import from sacct by themselves when the last
import is more than 15 minutes old. To import right away, for example because a
job just finished and you want it in a report, run:

```sh
stomp jobs sync
```

```text
Imported 2 job record(s) from sacct, covering 2026-11-15 18:03 EST to now. 77 job(s) in the catalog.
```

Each import starts three days before the previous one ended, to pick up records
that Slurm updated late, so the first number counts records read again as well
as new ones. If you've set `history_start` (below) earlier than anything
imported so far, the same import also fetches the older jobs. The second
number is every job stomp has. stomp asks sacct for a month at a time, to keep
the load on the accounting database low. If you've turned on the
[daily run](@/tutorials/automate.md), it imports accounting every day too.

## Report compute per grant

`stomp report compute` adds up the jobs per grant:

```sh
stomp report compute
```

```text
Compute by grant, 2026-10-19 to 2026-11-18 (jobs that ended in the period, plus running jobs; sacct synced 2026-11-18 18:03 EST)

GRANT          JOBS   NOT COMPLETED   CPU-HOURS   GPU-HOURS   BILLING-HOURS   EST. COST   ATTRIBUTED BY
NSF-2231         20               2       4,530         0.0           4,530       45.30   log 3, workdir 17
EPSCOR-T2         4               2         117         0.0             117        1.17   workdir 4
USDA-1180         1               0          31         7.7              31        2.23   workdir 1
(unassigned)      3               0          54         0.0              54        0.54   —
-----------------------------------------------------------------------------------------------------------
TOTAL            28               4       4,732         7.7           4,732       49.25
```

The first line gives the period, which is the last 30 days unless you say
otherwise, and when accounting was last imported. A job counts in the period in
which it ended; jobs still running are included with their hours so far.

- **JOBS** is how many jobs the grant had in the period.
- **NOT COMPLETED** counts the jobs among them that ended in any state other
  than `COMPLETED`: failed, timed out, cancelled, out of memory. Their hours
  are included in the other columns, because they used the allocation all the
  same. A high number here is worth a look.
- **CPU-HOURS** and **GPU-HOURS** are the sums of the CPU-H and GPU-H columns
  of `stomp jobs list`: allocated CPUs or GPUs times run time.
- **BILLING-HOURS** is Slurm's `billing` value for each job times its run
  time. Many sites weight it, so that a GPU or a large-memory node costs more
  than a plain CPU, and allocations are often charged in it. On Alex's cluster
  the billing value is simply the number of CPUs, so it matches CPU-HOURS. The
  column only appears if your site records billing.
- **EST. COST** is CPU-hours times `rate_per_cpu_hour` plus GPU-hours times
  `rate_per_gpu_hour`, from your config. It doesn't use billing hours. The
  column only appears if you've set at least one of the two rates (see
  [Set rates and compute options](#set-rates-and-compute-options) below).
- **ATTRIBUTED BY** counts the grant's jobs by how they were attributed, for
  example `log 3, workdir 17`. The three logged jobs are the `merge_nc` jobs
  tagged earlier.

Besides your grants, two more rows can appear. `(unassigned)` collects the
jobs nothing matched. `stomp` is stomp's own overhead: its
[scans](@/concepts/scans.md) run as Slurm jobs and tag themselves, shown as
`task` under ATTRIBUTED BY. Archive and restore tasks are counted toward the
grant whose data they handle, also as `task`. A tag with a grant that isn't in
your config gets its own row, such as `NSF2231 (unknown)`.

For a longer period, give the first and last day with `--from` and `--to`
(both included). `--by-month` splits each grant by the month its jobs ended:

```sh
stomp report compute --from 2026-09-01 --to 2026-10-31 --by-month
```

```text
Compute by grant, 2026-09-01 to 2026-10-31 (jobs that ended in the period, plus running jobs; sacct synced 2026-11-18 18:03 EST)

MONTH     GRANT          JOBS   NOT COMPLETED   CPU-HOURS   GPU-HOURS   BILLING-HOURS   EST. COST   ATTRIBUTED BY
2026-09   NSF-2231          9               3         963         0.0             963        9.63   workdir 9
2026-09   EPSCOR-T2        16               1         621          40             621       16.32   workdir 16
2026-09   USDA-1180         1               0          27         6.7              27        1.93   workdir 1
2026-09   (unassigned)      5               0          66         0.0              66        0.66   —
2026-10   NSF-2231         16               0       3,529         0.0           3,529       35.29   log 1, workdir 15
2026-10   EPSCOR-T2         4               1         122         6.3             122        2.80   workdir 4
2026-10   USDA-1180         1               0          35         8.7              35        2.52   workdir 1
2026-10   (unassigned)      5               0          67         0.0              67        0.67   —
---------------------------------------------------------------------------------------------------------------------
          TOTAL            57               5       5,429          62           5,429       69.81
```

A report can only include jobs stomp has imported. With the defaults, that
means nothing from before the first import's 90 days; `history_start`, below,
reaches further back.

On a machine where `sacct` isn't available, or when you don't want to wait for
it, `--no-sync` reports on what's already imported:

```sh
stomp report compute --from 2026-11-01 --no-sync
```

```text
Compute by grant, 2026-11-01 to 2026-11-18 (jobs that ended in the period, plus running jobs; sacct synced 2026-11-18 18:03 EST)

GRANT          JOBS   NOT COMPLETED   CPU-HOURS   GPU-HOURS   BILLING-HOURS   EST. COST   ATTRIBUTED BY
NSF-2231         14               2       3,129         0.0           3,129       31.29   log 2, workdir 12
EPSCOR-T2         4               2         117         0.0             117        1.17   workdir 4
USDA-1180         1               0          31         7.7              31        2.23   workdir 1
(unassigned)      1               0          17         0.0              17        0.17   —
-----------------------------------------------------------------------------------------------------------
TOTAL            20               4       3,294         7.7           3,294       34.87
```

Like the other reports, this one prints CSV, Markdown or JSON with `--format`;
[Report usage to your PI or department](@/tutorials/reports.md) shows how to
use them.

## Set rates and compute options

The cost columns come from rates in the `[general]` section of your config.
Alex's department charges back storage and compute, so the config has:

```toml
[general]
roots = ["~"]
rate_per_tb_month = 4.50     # what the department charges back, per TB-month
rate_per_cpu_hour = 0.01
rate_per_gpu_hour = 0.25
```

`rate_per_cpu_hour` and `rate_per_gpu_hour` drive EST. COST in the compute
report; `rate_per_tb_month` drives the storage costs in
[`stomp report history`](@/tutorials/reports.md). Rates are in whatever
currency you think in, must be zero or more, and are all optional: leave them
out and the cost columns disappear. stomp has no idea what your site really
charges, so the costs are only as good as the rates you enter.

Two settings in a `[compute]` section change how jobs are attributed and
imported:

```toml
[compute]
infer_from_workdir = true    # the default
history_start = 2026-01-01   # import jobs from this date on; default: 90 days back
```

- **`infer_from_workdir`** turns attribution by working directory on or off.
  With `false`, only tagged jobs have a grant and everything else is
  `(unassigned)`. Turn it off if your working directories don't say anything
  about grants, for example because every job runs from one shared code
  directory.
- **`history_start`** is how far back stomp imports accounting, if 90 days
  isn't enough. You can set it at any time: if it's earlier than what stomp
  has imported so far, the next import fills in the older jobs, a month at a
  time. How far back you can go depends on how long your site keeps
  accounting records.

The [configuration reference](@/reference/configuration.md#compute) lists every
key.

## What's next

- [Report usage to your PI or department](@/tutorials/reports.md) puts compute
  next to storage, for one grant or over time.
- [Close out a grant that has ended](@/tutorials/grant-closeout.md) checks for
  jobs that ran after a grant's end date.
- [Compute attribution](@/concepts/compute.md) explains the design in more
  depth.
- The [`stomp jobs`](@/reference/commands.md#stomp-jobs) and
  [`stomp report`](@/reference/commands.md#stomp-report) references list every
  option.
