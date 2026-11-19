+++
title = "How stomp measures storage"
description = "Instant sizes on CephFS, walking elsewhere, and what each number means."
weight = 20
+++

stomp has two ways to measure a directory, and picks one per root
automatically.

## On CephFS: instant

CephFS keeps running totals for every directory and exposes them as extended
attributes that any user can read:

| Attribute | Meaning |
|-----------|---------|
| `ceph.dir.rbytes` | Bytes in the whole subtree |
| `ceph.dir.rfiles` | Files in the whole subtree |
| `ceph.dir.rctime` | The newest change time anywhere in the subtree |
| `ceph.quota.max_bytes`, `ceph.quota.max_files` | A quota, if one is set on the directory |

You can read them yourself:

```sh
getfattr -n ceph.dir.rbytes ~/soil_carbon
```

So `stomp usage`, `stomp du` and `stomp tui` don't walk anything on CephFS:
they read one attribute per directory, and a home directory with tens of
millions of files is measured in a fraction of a second. stomp recognizes
CephFS by its filesystem type, or by being able to read `ceph.dir.rbytes`
(which covers `ceph-fuse` too).

**What the numbers mean.**

- **Sizes are logical file sizes**: what `ls -l` shows, added up. A sparse
  file counts at its full size. The extra raw space Ceph uses to keep copies
  of your data isn't included. That's the right number for attributing usage
  to grants.
- **The totals are updated lazily**, so they can lag a little behind recent
  writes. Reports print when they were measured. Nothing destructive in stomp
  ever relies on these totals; archiving checks every file's checksum before
  removing anything.
- **Snapshots aren't counted.** CephFS snapshots live in `.snap` directories,
  which stomp never enters and `rbytes` doesn't include.
- **Quotas** show in the QUOTA column as the limit and how much of it is used,
  or `none set`.

## Everywhere else: walking

On any other filesystem there are no running totals, so stomp walks the tree
and adds up file sizes itself, in parallel. That's what `du` does, and it
takes as long: seconds for thousands of files, much longer for millions.
stomp prints a note when it has to walk:

```text
stomp: ~ is not on CephFS, so measuring it means walking every file. This can take a while.
```

While walking, stomp never follows symlinks, never crosses into another
filesystem, and records directories it can't read instead of failing.

## The columns

| Column | Meaning |
|--------|---------|
| SIZE | Total logical size, in binary units (1 GiB = 1,024³ bytes). |
| FILES | Number of files. Directories aren't counted. |
| LAST CHANGE | How long ago anything in the tree last changed: the newest change time (ctime), which moves when a file is written, renamed, or has its permissions changed. |
| DIRS | How many directories a grant has. `2 of 3` means one doesn't exist. |
| QUOTA | The quota on the root's filesystem and how much is used, if there is one. |

## History

Every `stomp usage` (and every daily run, on CephFS) saves a **usage
sample**: the size and file count of each root and grant directory at that
moment. Samples are tiny and kept forever. They power:

- `stomp changes`: each grant now versus a past sample.
- `stomp report history`: storage per grant over time, and **TB-months**,
  storage multiplied by how long it was held, which is how many centers and
  cloud services charge.
- The info panel in `stomp tui`.

Use `stomp usage --no-record` when you don't want a sample saved, for
example from a script that runs often.

## Scans are a different thing

Measuring tells you *how much*. A [scan](@/concepts/scans.md) walks the tree
in a Slurm job and records *what kind*: how old the data is, what file types
and sizes, which directories are environments or caches. Reports such as
`stale` and the archiving policies need a scan; usage numbers don't.
