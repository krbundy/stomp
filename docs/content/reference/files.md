+++
title = "Files and directories"
description = "Where stomp keeps its config, catalog, tasks and logs."
weight = 30
+++

Everything stomp keeps is in your home directory, owned by you, and readable
only by you (directories are created with mode `0700`).

## The config

```text
~/.config/stomp/config.toml          the configuration (see Configuration)
~/.config/stomp/config.toml.bak      the previous version, after `stomp tui` edits it
```

`$XDG_CONFIG_HOME` moves `~/.config`. `--config PATH` uses another file for one
command.

## The data directory

```text
~/.local/share/stomp/
  catalog.db           SQLite database: measurements, scans, jobs, archives, proposals
  catalog.lock         lock file, so only one command imports at a time
  inbox/
    scans/             finished scan results waiting to be imported
    archives/          archive records from tasks
    jobs/              `stomp jobs log` records, one small file per job
  tasks/<task-id>/
    task.sh            the sbatch script stomp submitted (you can read or rerun it)
    slurm-<job>.out    the task's output
    state.json         status, Slurm job IDs, resubmissions
    progress.json      live progress, read by `stomp tasks`
  schedule/
    state.json         how the daily run is set up
    run.sh             the job script, in task mode
    logs/              one log per daily run; the newest 30 are kept
  manifests/<archive-id>/
                       the records of an archive whose files were restored while
                       a copy stays at a destination (see below)
```

`$XDG_DATA_HOME` moves `~/.local/share`.

**Only interactive commands write the catalog.** Tasks and `stomp jobs log`
write files to `inbox/`, and the next command that reads the catalog imports
them. SQLite's locking isn't reliable on network filesystems, so this keeps
the database safe even with many jobs running at once. The daily run is the
one exception: it opens the catalog for the few seconds its imports take.

**The catalog stays small.** Scans record totals per directory, never per
file, and directories under 1 MiB with fewer than 100 files are folded into
their parent. `stomp doctor` shows its size.

**Back it up if you care about history.** The catalog holds your usage
history, imported job accounting (which sites eventually purge from `sacct`)
and the list of archives. Archives themselves don't depend on it: each one
carries its own records (see below).

## Inside an archived directory

```text
~/soil_carbon_runs/2024_calibration/
  STOMP_ARCHIVED.txt                        a note: what happened, when, and how to restore
  .stomp-archive/NSF-2231-20261118-bde0da/
    chunk-0001.tar.zst                      the data (local archives only)
    manifest.jsonl.zst                      every file, with its checksum
    SHA256SUMS                              checksums of the chunks and manifest
    README.txt                              how to restore without stomp
    settings.json                           how this archive was made
    journal.jsonl                           every step taken, for resuming
```

For an archive at a remote destination, the chunks live there and everything
else stays here, so `stomp archives show` and planning a restore work
offline. See [Archive format](@/reference/archive-format.md).

When you restore all of a remote archive, the directory goes back to normal
and the copy at the destination is kept. Its records (everything above except
chunks, and the manifest only if it's 64 MiB or less) move to
`~/.local/share/stomp/manifests/<archive-id>/`, so you can still list, check
and restore from that copy. `stomp archives delete <archive-id>` deletes the
copy and these records.

## At a destination

```text
<path or prefix>/<grant>/<archive-id>/
  chunk-0001.tar.zst
  chunk-0002.tar.zst
  …
  manifest.jsonl.zst
  SHA256SUMS
  README.txt
```

`<grant>` is `unassigned` for directories outside any grant. The manifest is
uploaded last, so an archive directory without one is unfinished.

## Removing stomp

stomp has no daemon or system files. To remove it:

1. `stomp schedule disable`, if you enabled the daily run.
2. Restore anything you want back from its archives (`stomp archives` lists
   them). Archives stay restorable without stomp, using their `README.txt`.
3. Delete `~/bin/stomp`, `~/.config/stomp` and `~/.local/share/stomp`.
