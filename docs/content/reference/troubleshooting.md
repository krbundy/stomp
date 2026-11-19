+++
title = "Troubleshooting"
description = "Common problems and how to fix them."
weight = 60
+++

Start with `stomp doctor`. It checks the config, the data directory, the
filesystem, Slurm, the daily schedule and every destination, and says what to
do about each problem. `stomp doctor --submit` runs the same checks on a
compute node.

## The config

### "is not inside any of general.roots"

```text
stomp: error: grant NSF-2231: ~/project is not inside any of general.roots (it resolves to /project/rivera/alex; is it a symlink?)
```

Grant directories must be inside a root, and stomp follows symlinks to their
real location first. If `~/project` is a link into another filesystem, add
that filesystem as a root:

```toml
[general]
roots = ["~", "/project/rivera/alex"]
```

### "a directory can belong to only one grant" or "grant directories must not overlap"

Two grants claim the same directory, or one grant's directory is inside
another's. Every file must count toward exactly one grant. Split the work
into separate directories, or pick the grant that should own the parent. See
[Grants, roots and directories](@/concepts/grants-and-roots.md).

### "unknown field"

The config has a key stomp doesn't know, often a typo or a key in the wrong
section. The message shows the line and lists the keys that are allowed there:

```text
stomp: error: ~/.config/stomp/config.toml: TOML parse error at line 38, column 1
   |
38 | dir  = ["~/forest_plots"]
   | ^^^
unknown field `dir`, expected one of `id`, `title`, `pi`, `start`, `end`, `dirs`, `track_files`
```

## Measuring

### "~ is not on CephFS, so measuring it means walking every file"

stomp gets instant sizes from CephFS. Anywhere else, `stomp usage`, `stomp du`
and `stomp tui` have to walk the tree, which takes a while on millions of
files. That's expected; the message is a reminder. On a shared login node,
prefer a [scan task](@/reference/commands.md#stomp-scan) for big trees.

### The numbers look slightly behind

CephFS updates its directory totals lazily, so sizes can lag a few seconds to
minutes behind recent writes. Reports print when they were measured. Nothing
destructive ever relies on these totals.

## Tasks

### A task is stuck pending, or failed right away

```sh
stomp tasks show <ID>
```

shows the Slurm job and where its log and script are. The most common causes
are a missing or wrong `account` or `partition` in `[slurm]`, or a time limit
above what the partition allows. Fix the config and run the command again.
The script stomp submitted is kept in `~/.local/share/stomp/tasks/<ID>/task.sh`.

### A scan or archive ran out of time

Tasks save their progress five minutes before the time limit and resubmit
themselves (up to 5 times). If that's off (`slurm.auto_resubmit = false`) or
used up, continue by hand:

```sh
stomp scan --resume
stomp archive --resume ~/soil_carbon_runs/2024_calibration
```

Give the next attempt more time with `--time`, or set `slurm.time`.

## Archives

### "only … is free under the quota"

Archiving needs room for the chunk it's writing (up to `chunk_size`) until
the originals in it are removed. Free some space, use a smaller
`--chunk-size`, or, for a remote destination, set `general.staging_dir` to
scratch space so chunks are built there.

### "is already archived" or "was started earlier"

The directory has a `STOMP_ARCHIVED.txt`. If an earlier archive stopped
partway, the note says so. Either continue it with `stomp archive --resume
DIR`, or undo it with `stomp restore DIR`: restoring everything from an
unfinished archive puts back what was archived so far and removes the
archive, so you can start again. If its task is still queued or running,
cancel it first with `stomp tasks cancel ID`.

### Getting rid of a copy kept at the NAS or in Spaces

After you restore all of a remote archive, the copy at the destination is
kept, as a backup. `stomp archives` lists it as `restored-copy-kept`. When you
don't need it anymore:

```sh
stomp archives delete <ID>
```

### "N file(s) already existed and were left alone"

A restore never replaces existing files unless you pass `--overwrite`. Either
move the existing files aside, restore with `--overwrite`, or restore
somewhere else with `--to DIR`. `stomp restore ID --dry-run` lists the
conflicts before you start. The archive is kept until a restore of everything
into the original place completes with no conflicts. The command exits with
code 3, and a restore task shows as `partial`.

### Some files weren't removed after archiving

Files that changed while being archived are packed but kept in place, and
listed as `kept`. So are files owned by other people, environments and
caches. `stomp archives show ID` lists what was left and why. Archive the
directory again later if you want the rest.

## Destinations

### "can't log in to … without a password prompt"

stomp runs `ssh` and `sftp` with `BatchMode=yes`, so they never stop to ask for
a password. Make sure `sftp <host>` works with an SSH key, and that the key is
usable in a batch job: an ssh-agent on your laptop isn't available to a Slurm
task. A key on the cluster, set up for the NAS, is the usual answer. Ask
whoever runs the NAS if you're unsure.

### It works from the login node but not in a task

Compute nodes often can't reach the internet or other networks. Check with:

```sh
stomp doctor --submit
```

If a destination fails there, split the work: archive locally in a task, then
move the archive from a login or data-transfer node:

```sh
stomp archive ~/marsh_mapping --to local
stomp archives move <ID> --to nas --here
```

### "the server accepted uploads with wrong checksums"

Your S3 service doesn't enforce request checksums, so `verify =
"server-checksum"` can't prove uploads arrived intact. Set `verify =
"readback"` for that destination (each object is downloaded and hashed after
upload; this costs download traffic).

## Compute

### Jobs show up as unassigned

```sh
stomp jobs list --unassigned
```

A job is attributed by `stomp jobs log` in its script, or by its working
directory being inside a grant directory. Jobs submitted from elsewhere (your
home, scratch) need one of:

```sh
stomp jobs log NSF-2231 --job 4801234     # tag one past job
stomp jobs retag NSF-2231 4801234 4801240 # or several at once
```

### "sacct was not found"

Compute attribution reads Slurm accounting with `sacct`. Run stomp on a node
where Slurm's commands are available (usually any login node).

## The daily run

### "scrontab isn't available"

Your site doesn't enable `scrontab`. `stomp schedule enable` falls back to a
job that queues the next one; you can ask for it directly with `--mode task`.

### `stomp schedule status` reports a problem

It checks that the scrontab entry or the queued job still exists and that a
run finished in the last two days. Each run's log is in
`~/.local/share/stomp/schedule/logs/`. Running `stomp schedule enable` again
repairs a missing entry or job.

## The browser

### `stomp tui` says it needs a terminal

It won't run with its output redirected or inside a batch job. For scripts,
use `stomp du` or `stomp usage`.

### The screen looks wrong over SSH

Make sure `TERM` is set to something your terminal supports (for example
`xterm-256color`). Resizing the window redraws it, and so does `R`.
