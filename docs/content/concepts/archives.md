+++
title = "How archives work"
description = "Chunks, checksums, the journal, and the rules stomp follows before removing anything."
weight = 50
+++

`stomp archive DIR` packs a directory into a few large compressed files and
removes the originals. That saves space (especially for CSV, text and
uncompressed rasters), and it turns millions of small files into a handful,
which the metadata server and your file quota both appreciate.

Removing originals is the one dangerous thing stomp does, so it follows strict
rules. This page explains them.

## What an archive is

- **Chunks**: tar files compressed with zstd, `chunk-0001.tar.zst` and on,
  each up to `archive.chunk_size` (50 GiB by default). Each chunk can be
  extracted on its own.
- **A manifest**: every file's path, size, mode, times and BLAKE3 checksum,
  and which chunk it's in.
- **`SHA256SUMS`**, **`README.txt`** and a **`STOMP_ARCHIVED.txt`** note in the
  directory, explaining what happened and how to restore with or without
  stomp.

By default everything stays inside the directory, in
`.stomp-archive/<ID>/`, and the usage stays with the directory's grant. With
`--to nas` or another [destination](@/concepts/destinations.md), the chunks
leave the cluster and only the records stay. See
[Archive format](@/reference/archive-format.md) for the details.

## The steps

stomp first makes a **plan**: the list of files and which chunk each goes in,
in sorted path order. The plan is saved and fixed. Files created after it
starts aren't in it, so they simply stay where they are.

Then, for each chunk in turn:

1. **Write.** Pack the chunk's files into the tar, compressing as it goes.
   Every file's contents are hashed (BLAKE3) while they're read, and the chunk
   is hashed (SHA-256) while it's written.
2. **Verify.** Read the chunk back, decompress it completely, and compare every
   file's hash with the one recorded while writing. A chunk that doesn't match
   is discarded and written again; nothing from it is removed.
3. **Upload and verify again** (remote destinations only). The chunk is
   checked at the destination, then the local copy is deleted.
4. **Remove the originals.** Each original is checked one last time: its
   inode, size, modification time and change time must be exactly what they
   were when it was packed. Only unchanged files are removed.

When every chunk is done, now-empty directories are removed and the final
manifest, `SHA256SUMS`, `README.txt` and note are written.

Because each chunk is finished before the next begins, archiving needs only
about one chunk's worth of extra space, not a copy of the whole directory.
Before starting, stomp checks the quota (on CephFS) and refuses if there
isn't room.

## The rules

| | stomp promises |
|-|----------------|
| 1 | No file is removed until the chunk holding it has been verified at its final location. |
| 2 | Just before removing a file, stomp checks it again. If its size, modification time or change time changed since it was packed, it's kept in place and listed as *kept*. |
| 3 | A directory is removed only when it's empty. stomp never deletes a tree recursively. |
| 4 | Every step is written to a journal before and after it happens. An interrupted archive resumes from the journal and never repeats a deletion or upload. |
| 5 | Archiving asks for confirmation first (`--yes` skips it), and `--dry-run` and `--estimate` change nothing. |
| 6 | stomp changes nothing outside the directory being archived, its own data and staging directories, and its own paths at destinations. |

The journal is tested by stopping an archive after every single step in
turn, resuming it, restoring it, and comparing every file, mode, time and
link with the original.

## What's left in place

- **Software environments and caches** inside the directory: rebuild or clean
  them instead.
- **Files owned by other people**, unless you set `archive.only_owned = false`.
  stomp never removes a collaborator's files from a shared directory.
- **Sockets, FIFOs, device files**, other filesystems mounted inside, and files
  stomp can't read.
- **Files that changed** while being archived (rule 2).

All of these are listed, with the reason, in the manifest and in
`stomp archives show`.

## What stomp refuses to archive

- A root itself.
- A directory that contains another grant's directories. Archive each grant's
  directories separately, so each archive belongs to one grant.
- An environment, a cache, or a directory matching `scan.exclude`.
- A directory holding stomp's own data, or anything inside a `.stomp-archive`.
- A directory that's already archived (continue an unfinished one with
  `--resume`).

## Estimating first

```sh
stomp archive ~/soil_carbon_runs/2024_calibration --estimate
```

prints the plan, then compresses a sample of the files and predicts the
compressed size, how many files you'll have afterwards, and roughly how long
it will take. Nothing changes. zstd stores data that won't compress (JPEGs,
already-compressed GeoTIFFs) quickly without trying hard, so there's no need
to sort files by type first.

## Restoring

```sh
stomp restore NSF-2231-20261118-bde0da
```

- **Only the chunks that are needed** are read (or downloaded), so restoring
  one subdirectory with `--only` is quick.
- **Every chunk's SHA-256** is checked first, and every file's BLAKE3 hash after
  extraction.
- **Files go back where they were**, with their modes and modification times.
  Existing files are never replaced unless you pass `--overwrite`; `--to DIR`
  restores somewhere else instead.
- **A restore can be rerun.** Files that already match are counted as
  restored, so an interrupted restore just continues.
- **`--dry-run` shows the plan first**: files, chunks to read or download,
  what's already in place, and any conflicts.
- **After everything is back** in the original place with no conflicts, the
  local archive is removed. A remote copy is kept unless you pass
  `--delete-remote`: its records move to stomp's data directory, so you can
  still list, check and restore from it, and `stomp archives delete ID`
  removes it when you no longer need it.

## Undoing an unfinished archive

If an archive stopped partway (out of time, cancelled), you can either finish
it with `stomp archive --resume DIR` or undo it with `stomp restore DIR`.
Restoring everything from an unfinished archive puts back what was archived
so far, while the rest never left. Then stomp removes the archive's chunks,
any it had uploaded, and its records and note, so the directory is exactly as
it was and can be archived again. stomp won't do this while the archive's own
task is still queued or running; cancel it first.

## Archiving is not backup

Archiving *moves* data. After archiving to a NAS or to object storage, that
copy is the **only** copy. Find out whether your NAS or bucket is itself
backed up before you archive irreplaceable data to it.
