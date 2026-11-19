+++
title = "Destinations"
description = "Where archive chunks can live, and how each place verifies them."
weight = 60
+++

A **destination** is a named place outside your home directory where archive
chunks can be stored: an SFTP server such as a lab NAS, or S3-compatible
object storage such as DigitalOcean Spaces. You define them in the config and
refer to them by name:

```sh
stomp archive ~/marsh_mapping --to nas
stomp archives move USDA-1180-20261118-4c19e2 --to spaces
```

`local` is the built-in destination meaning "inside the directory itself".
It's the default unless you set `archive.default_destination`.

## The same layout everywhere

```text
<base>/<grant>/<archive-id>/
    chunk-0001.tar.zst
    chunk-0002.tar.zst
    …
    manifest.jsonl.zst
    SHA256SUMS
    README.txt
```

`<base>` is the SFTP `path`, or the S3 bucket plus `prefix`. `<grant>` is
`unassigned` for directories outside any grant. Anyone with access can
restore an archive from there with `sha256sum`, `zstd` and `tar`, following
its `README.txt`.

The manifest is uploaded **last**, after every chunk has been verified. An
archive directory without a manifest is unfinished, so a crash never leaves
something that looks complete but isn't.

## What stays on the cluster

When an archive goes to a destination, only the chunks leave. The records
(manifest, `SHA256SUMS`, `README.txt`, settings, journal) stay in
`.stomp-archive/<ID>/` inside the archived directory, next to the
`STOMP_ARCHIVED.txt` note that says where the data went. So `stomp archives`,
`stomp archives show` and planning a restore work without the network, and
anyone browsing the directory can see what happened.

Chunks are built inside the archived directory before upload, or in
`general.staging_dir` if you set one (useful when your quota is tight).

## SFTP

stomp runs your system's `ssh` and `sftp` programs rather than its own SSH
code. So everything in your `~/.ssh/config` works: host aliases,
`ProxyJump`, keys, `known_hosts`. stomp always runs them with
`BatchMode=yes`: if login needs a password, it fails at once instead of
hanging in a batch job.

**Uploads** go to `<file>.partial` and are renamed into place only after
they're verified. An interrupted upload starts that file over. Files go one at
a time.

**Paths.** A `path` starting with `~` means your home directory on the server,
not on the cluster: `path = "~/stomp"` works without knowing where the NAS
keeps home directories.

**Verification**, set with `verify`:

| Mode | How it checks | When |
|------|---------------|------|
| automatic (default) | Runs `sha256sum` on the server over `ssh`; if the server only allows SFTP, downloads the file again and hashes it | Works with any server |
| `remote-sha256` | `sha256sum` over `ssh` only; an error if the server refuses | You want to be sure nothing is downloaded twice |
| `readback` | Downloads every file again and hashes it | SFTP-only servers |
| `size` | Compares sizes only | Weak; avoid |

`stomp doctor` tells you which applies to your server.

## S3 and DigitalOcean Spaces

stomp has its own small S3 client, built in, so it needs no AWS tools. It
reads keys from `~/.aws/credentials` (or the `AWS_*` environment variables)
and talks to AWS or any S3-compatible service through `endpoint`.

**Uploads.** A small chunk goes up in one request. A large one is uploaded in
parts of at least 64 MiB, several at once (`parallel_uploads`, 4 by default).
An interrupted multipart upload resumes: stomp asks the server which parts it
already has and sends only the rest. Busy-server responses (`503 SlowDown`,
`429`) and network errors are retried with backoff.

**Integrity.** Every request is signed with the SHA-256 of its body and
carries a `Content-MD5` header, so the server itself rejects anything damaged
on the way. Each object also records its own SHA-256 in its metadata.

**Verification**, set with `verify`:

| Mode | How it checks |
|------|---------------|
| `server-checksum` (default) | The server checks each request's checksums on arrival; afterwards stomp checks the object's size and recorded SHA-256. |
| `readback` | Downloads each object and hashes it. Costs download traffic, which many providers charge for. |
| `size` | Sizes only. Weak; avoid. |

Some services silently ignore checksum headers. `stomp doctor` tests yours
by uploading a small object with deliberately wrong checksums: if the server
accepts it, `doctor` fails and tells you to use `verify = "readback"`.

**Spaces specifics.** Use the region endpoint, for example
`https://nyc3.digitaloceanspaces.com`; `region` doesn't matter. Spaces keys
don't expire, so they work in long tasks. Spaces has no cold storage classes.

Cold AWS storage classes (Glacier, Deep Archive) aren't supported yet;
`storage_class` accepts only `"STANDARD"`. A bucket lifecycle rule that aborts
incomplete multipart uploads after a few days is a good backstop for archives
that are interrupted and never resumed or undone.

## Can your compute nodes reach it?

A destination you can reach from a login node may be unreachable from compute
nodes, which often have no internet access. Check:

```sh
stomp doctor --submit
```

This runs the same checks in a short task on a compute node. If a
destination fails there, split the work:

1. Archive locally in a task: `stomp archive DIR --to local`.
2. From a login or data-transfer node, move it:
   `stomp archives move ID --to nas --here`. This step mostly waits on the
   network, not the CPU.

## Moving and restoring

- **`stomp archives move ID --to DEST`** moves an archive's chunks between its
  directory and a destination, or between two destinations. Each chunk is
  copied and verified at the new place before any copy of it is removed.
- **`stomp restore ID`** downloads only the chunks it needs, verifies them,
  and extracts them. The remote copy is **kept** afterwards, unless you pass
  `--delete-remote`. Its records move to `~/.local/share/stomp/manifests/ID/`,
  so `stomp archives show`, `verify` and `restore` keep working on the copy
  (a manifest bigger than 64 MiB is fetched from the destination when it's
  needed, rather than kept in your quota).
- **`stomp archives delete ID`** deletes a kept copy when you no longer want
  it.

**How stomp deletes at a destination.** Whether it's a move, `--delete-remote`,
undoing an unfinished archive or `archives delete`, stomp removes the manifest
first, so an interrupted deletion never leaves something that looks like a
complete archive, and aborts any unfinished S3 multipart uploads under the
archive, which S3 otherwise keeps (and bills) indefinitely.
