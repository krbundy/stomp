+++
title = "Archive format"
description = "What an archive contains, and how to restore one without stomp."
weight = 40
+++

A stomp archive is a handful of ordinary files: compressed tar files and a few
text records. You never need stomp to get your data back. Every archive
carries a `README.txt` with the exact commands, and they use only
`sha256sum`, `zstd` and `tar`.

## The files

| File | What it is |
|------|------------|
| `chunk-0001.tar.zst`, `chunk-0002.tar.zst`, … | The data: POSIX tar files (PAX format, for long names and exact timestamps), compressed with zstd. Each chunk stands alone and can be extracted by itself. |
| `manifest.jsonl.zst` | One JSON record per line: a header, then one record for every file, directory and link, then what was left in place. |
| `SHA256SUMS` | The SHA-256 of every chunk and of the manifest, in `sha256sum -c` format. |
| `README.txt` | Where the archive came from and how to restore it, with and without stomp. |
| `settings.json` | How the archive was made: destination, chunk size, compression level. Kept with the directory. |
| `journal.jsonl` | Every step stomp took, one per line. Kept with the directory; it's how an interrupted archive resumes. |

Inside the directory they're in `.stomp-archive/<archive-id>/`, next to a
`STOMP_ARCHIVED.txt` note. At a destination, the chunks, manifest,
`SHA256SUMS` and `README.txt` are in `<base>/<grant>/<archive-id>/`.

**Archive IDs** are `<grant>-<yyyymmdd>-<6 hex digits>`, for example
`NSF-2231-20261118-bde0da`, or `unassigned-…` for a directory outside any
grant.

## How files are packed

- Paths inside the tar files are relative to the archived directory.
- Files are packed in sorted path order, so a subdirectory lands in as few
  chunks as possible, and restoring part of an archive reads only a few
  chunks.
- A file is never split across chunks. A file larger than the chunk size gets
  a chunk of its own.
- Symlinks are stored as links and never followed. Hard links within the
  directory are kept as hard links.
- Modes and modification times are stored; restored files belong to whoever
  restores them.
- Left in place, and listed in the manifest's `skipped` records with the
  reason: software environments, caches, directories matching `scan.exclude`,
  stomp's own data, other archives, files owned by other users (unless
  `archive.only_owned = false`), sockets, FIFOs and device files, other
  filesystems mounted inside, and unreadable files.
- A file that changed while it was being archived is packed but never
  removed; its entry is marked `"kept": true`.

## The manifest

```sh
zstdcat manifest.jsonl.zst | head -3
```

```text
{"type":"header","format":"stomp-archive","version":1,"archive_id":"NSF-2231-20261118-bde0da","source":"/home/alex/soil_carbon_runs/2024_calibration","grant":"NSF-2231","user":"alex","host":"login1","stomp_version":"0.1.0","created_at":"2026-11-18T15:36:28.967886274Z","finished_at":"2026-11-18T15:36:29.257839004Z","zstd_level":3,"chunk_size":53687091200,"totals":{"files":36,"bytes":2525772,"dirs":6,"symlinks":0,"hardlinks":0,"chunks":1,"compressed_bytes":840190,"kept":0,"skipped":0,"skipped_bytes":0},"destination":"local","location":null}
{"type":"entry","path":"run_00","kind":"dir","chunk":1,"size":0,"mode":509,"mtime":1795015904,"uid":1000,"gid":1000,"ino":23092874,"mtime_ns":1795015904253779147,"ctime_ns":1795015904253779147}
{"type":"entry","path":"run_00/model.log","kind":"file","chunk":1,"size":3200,"mode":436,"mtime":1762183904,"uid":1000,"gid":1000,"blake3":"6eacfd83221a07741f54887ff7df35a779ba5f3e9c2861ce86cc88a2dd4b69c7","ino":23092881,"mtime_ns":1762183904170745611,"ctime_ns":1795015904253801052}
```

- **The header** records the archive ID, the original path, grant, user,
  host, stomp version, times, settings and totals.
- **Entries** have the relative `path` (base64-encoded if it isn't valid
  UTF-8), `kind` (`file`, `dir`, `symlink`, `hardlink`), the `chunk` it's in,
  `size`, `mode`, `mtime`, owner, and for files a BLAKE3 checksum of the
  contents.
- **`skipped` records** list what stayed in place, and why. Entries marked
  `"kept": true` were archived but their originals were left alone because
  they changed.

With `jq` you can answer questions without extracting anything:

```sh
# Which chunk holds a file?
zstdcat manifest.jsonl.zst | jq -r 'select(.path == "run_03/output_2.csv") | .chunk'

# The ten largest files
zstdcat manifest.jsonl.zst | jq -r 'select(.kind == "file") | "\(.size)\t\(.path)"' | sort -rn | head
```

## Restoring without stomp

From the archive's directory (`.stomp-archive/<id>/`, or a copy downloaded
from the destination):

```sh
sha256sum -c SHA256SUMS
for c in chunk-*.tar.zst; do zstd -dc "$c" | tar -x -C /path/to/restore/into; done
```

The first command proves every chunk is intact. The loop extracts each chunk
in turn. To get one file, find its chunk in the manifest and extract just that
path:

```sh
zstd -dc chunk-0002.tar.zst | tar -x -C /tmp/restore run_03/output_2.csv
```

The `README.txt` in each archive has these commands with the right paths
already filled in.

## Integrity

- While packing, stomp hashes every file's contents (BLAKE3) as it reads them,
  and every chunk (SHA-256) as it writes it.
- Before anything is removed, each chunk is read back, fully decompressed, and
  every file's hash compared. See [How archives work](@/concepts/archives.md).
- `stomp archives verify ID` checks every chunk's SHA-256 again, wherever it
  is; `--deep` also decompresses each chunk and checks every file.
