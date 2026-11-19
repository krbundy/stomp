+++
title = "Quick start"
description = "From nothing to storage per grant in about ten minutes."
weight = 20

[extra]
time = "10 minutes"
needs = "stomp on your PATH (see Install)"
+++

You'll write a config that says which of your directories belong to which
grant, check it, and get your first per-grant usage report. Run these commands
on a login node.

## Create a config

```sh
stomp init
```

```text
Wrote ~/.config/stomp/config.toml with 8 grant stubs, one per top-level directory.
Next: edit it to define your grants, check it with `stomp doctor`, then run `stomp usage`.
```

`stomp init` looks at the top-level directories of your home and writes a
commented-out `[[grant]]` block for each one. On CephFS it shows each
directory's size so you can see where your space goes. It never overwrites an
existing config.

## Say which directories belong to which grant

Open `~/.config/stomp/config.toml` in an editor. For each grant, uncomment a
block (or write a new one) and fill it in. A grant can have several
directories:

```toml
[[grant]]
id    = "NSF-2231"
title = "Soil carbon dynamics under forest management"
pi    = "J. Rivera"
start = 2024-09-01
end   = 2027-08-31
dirs  = ["~/soil_carbon", "~/soil_carbon_runs"]

[[grant]]
id    = "USDA-1180"
title = "Coastal marsh mapping"
end   = 2026-06-30
dirs  = ["~/marsh_mapping"]

[[grant]]
id    = "EPSCOR-T2"
title = "Forest plot remote sensing"
end   = 2028-05-31
dirs  = ["~/forest_plots"]
```

Delete the stubs you don't need. Everything under a grant's directories counts
toward it; anything else in your home is reported as unassigned. Dates are
written without quotes.

While you're there, uncomment `[slurm]` and set at least your `account`, so
the scans and archives stomp submits are charged correctly:

```toml
[slurm]
account   = "lab-rivera"
partition = "standard"
```

## Check it

```sh
stomp doctor --offline
```

```text
ok    config       ~/.config/stomp/config.toml: 3 grant(s), 1 root(s), 0 destination(s)
ok    data dir     ~/.local/share/stomp is writable
ok    catalog      88.0 KiB: 0 measurement(s), 0 scan(s), 0 job record(s)
info  accounting   not imported yet; `stomp jobs sync` does it
ok    slurm        sbatch, squeue and sacct found
ok    slurm        scrontab found; recurring work can use it if the site allows
info  threads      8 worker thread(s)
info  schedule     daily runs are off; `stomp schedule enable` keeps usage, accounting, scans and proposals current

No problems found.
```

If something is wrong, such as a typo in a path or two grants claiming the
same directory, `doctor` says exactly what and where. Fix it and run it again.

## See your usage per grant

```sh
stomp usage
```

```text
Measured 2026-11-18 10:38 EST

ROOT   FILESYSTEM      SIZE   FILES   QUOTA
~      CephFS       1.7 TiB   1,381   none set

GRANT          DIRS        SIZE   FILES   LAST CHANGE
USDA-1180         1   913.9 GiB     457   141d ago
NSF-2231          2   592.5 GiB     166   2h ago
EPSCOR-T2         1   179.2 GiB      46   3d ago
(unassigned)      —    74.9 GiB     712   just now
-----------------------------------------------------
TOTAL                   1.7 TiB   1,381   just now
```

On CephFS this is instant, because the filesystem keeps running totals for
every directory. On other filesystems stomp walks every file and tells you so
first. Each run is saved, so later you can see how usage changed with
`stomp changes` and add it up over time with `stomp report history`.

## See where the space is

```sh
stomp du ~
```

```text
     SIZE   FILES   LAST CHANGE   GRANT                                     PATH
  1.7 TiB   1,381   just now      contains NSF-2231, USDA-1180, EPSCOR-T2   ~
913.9 GiB     457   141d ago      USDA-1180                                   marsh_mapping/
509.6 GiB      66   2h ago        NSF-2231                                    soil_carbon_runs/
179.2 GiB      46   3d ago        EPSCOR-T2                                   forest_plots/
 82.8 GiB     100   5h ago        NSF-2231                                    soil_carbon/
 59.0 GiB       2   2d ago        —                                           scratch/
  7.3 GiB     501   120d ago      —                                           miniconda3/
  5.0 GiB      82   260d ago      —                                           containers/
  3.5 GiB     120   90d ago       —                                           .cache/
…
```

`stomp du` is like `du`, with the grant each directory counts toward. Use
`-d 2` to go a level deeper, or `stomp tui` to browse interactively.

## Attribute your compute jobs

Most jobs need nothing: a job whose working directory is inside a grant's
directory counts toward that grant. For the others, add one line to the job
script:

```sh
#!/bin/bash
#SBATCH --time=04:00:00
stomp jobs log NSF-2231
srun ./model
```

`stomp jobs log` never makes a job fail. Then:

```sh
stomp report compute
```

shows CPU-hours and GPU-hours per grant for the last 30 days.

## Where to go next

- [Find what's using your space](@/tutorials/find-space.md) with a detailed
  scan, stale-data and cache reports.
- [Archive a finished project](@/tutorials/archive.md) to free space safely.
- [Automate archiving with policies](@/tutorials/automate.md) and turn on the
  daily run with `stomp schedule enable`.
- Read [Grants, roots and directories](@/concepts/grants-and-roots.md) for the
  rules behind the config.
