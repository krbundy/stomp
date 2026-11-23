+++
title = "Compute attribution"
description = "How stomp decides which grant each Slurm job belongs to."
weight = 80
+++

Most centers record compute per user, not per grant. stomp reads your jobs
from Slurm's accounting (`sacct`) and decides which grant each one belongs
to, so you can report CPU-hours and GPU-hours per grant next to storage.

## How a job gets its grant

stomp looks for an answer in this order:

1. **An explicit tag.** There are three kinds:
   - a log from inside the job, written by `stomp jobs log GRANT` in the job
     script;
   - a tag you set by hand afterwards, with `stomp jobs log GRANT --job ID` or
     `stomp jobs retag GRANT JOB...`;
   - a tag from stomp's own tasks: an archive or restore task tags itself with
     the grant whose data it handles, and scans are tagged `stomp`.
2. **The working directory.** If the directory the job ran in, as `sacct`
   recorded it, is inside a grant's directory, the job belongs to that grant.
   Turn this off with `compute.infer_from_workdir = false`.
3. **Otherwise the job is unassigned.**

When a job is tagged more than once, a tag set by hand or by a task replaces
the earlier one, but a log from inside the job never replaces those two, so a
requeued job can't undo your fix. `stomp jobs untag` removes explicit tags,
and the job falls back to its working directory.

Reports show how jobs were attributed in the ATTRIBUTED BY column: `log` for
explicit tags (from a job script or by hand), `task` for stomp's tasks, and
`workdir` for inference. So you can see how much rests on inference.

## Logging from a job script

```sh
#!/bin/bash
#SBATCH --job-name=century
#SBATCH --time=12:00:00
stomp jobs log NSF-2231 --note "2025 scenarios"
srun ./century scenario.yaml
```

- **It records** the job ID from `SLURM_JOB_ID`, the grant, the time, the
  host and your optional note.
- **Job arrays** are logged once for the whole array (by
  `SLURM_ARRAY_JOB_ID`). Later tasks in the array see the record and do
  nothing.
- **It's fast and safe at scale.** It writes one small file to stomp's inbox
  and returns in milliseconds, without opening the catalog or taking a lock.
  Thousands of array tasks can run it at once.
- **It never breaks your job.** It always exits 0 and prints problems as
  warnings. A grant ID it doesn't recognize is still recorded, and shows in
  reports as an unknown grant, so a typo is visible and fixable with
  `stomp jobs retag`. Pass `--strict` if you'd rather it fail: problems are
  then errors, it exits 1, and an unknown grant isn't recorded.
- **Notes show up** in `stomp jobs list`, in a NOTE column.

## Importing accounting

stomp copies your job records from `sacct` into its catalog:

- **Automatically**: any command that shows compute imports first if the last
  import is more than 15 minutes old, and so does the daily run.
  `stomp jobs sync` does it on demand.
- **In bounded windows**, a month at a time, so filling in history doesn't
  load the shared accounting database with one huge query.
- **From `compute.history_start`** on the first import, or 90 days back if
  that's not set. If you set `history_start` earlier later on, the next import
  fills in the older jobs.
- **For keeps.** Sites purge old accounting records eventually; once copied,
  stomp's reports don't depend on `sacct` keeping them.

It asks only for your own jobs, one line per allocation (no job steps), with
these fields: `JobID`, `JobName`, `State`, `Submit`, `Start`, `End`,
`ElapsedRaw`, `CPUTimeRAW`, `AllocTRES`, `Partition`, `Account` and `WorkDir`.
Pending jobs are skipped; running jobs are shown as in progress and updated
when they finish.

## What's counted

| Metric | From |
|--------|------|
| CPU-hours | `CPUTimeRAW / 3600`: allocated CPUs × elapsed time |
| GPU-hours | GPUs in `AllocTRES` (`gres/gpu=N`) × elapsed hours |
| Billing hours | `billing=` in `AllocTRES` × elapsed hours, if your site records it |
| Jobs not completed | jobs that failed, timed out or were cancelled |

Failed and cancelled jobs used their allocation too, so they're counted; the
NOT COMPLETED column shows how many there were. Set `rate_per_cpu_hour` and
`rate_per_gpu_hour` in `[general]` to see an estimated cost.

## Moving directories between grants

Attribution by working directory is computed from the current config each
time. If you move a directory from one grant to another, jobs inferred from
it move too, just as its storage history does. Jobs tagged explicitly keep
their grant.
