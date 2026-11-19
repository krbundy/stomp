+++
title = "Send archives to a NAS or Spaces"
description = "Move archives off the cluster over SFTP or to S3-compatible storage, verified at the other end."
weight = 70

[extra]
time = "30 minutes"
needs = "a working config, an account on the NAS or a Spaces bucket, and the previous tutorial"
+++

An archive inside your home directory still takes up quota. In this tutorial
you set up the lab NAS as a destination, check it from a login node and from a
compute node, archive a directory straight to it, move an existing archive
there and back, and restore from it. The last part sets up DigitalOcean Spaces,
or any other S3-compatible storage, the same way.

## Make sure you can log in without a password

stomp talks to an SFTP server with the system's own `sftp` and `ssh` programs,
so it uses your `~/.ssh/config`, your keys and your `known_hosts` as they are.
It always runs them with `BatchMode=yes`: it never asks for a password or a
passphrase. If a login would need one, it fails straight away instead of
leaving a Slurm task waiting at a prompt nobody will answer.

So first make sure `sftp` reaches the NAS with an SSH key. A host alias in
`~/.ssh/config` keeps the stomp config short:

```text
Host nas
    HostName nas.lab.example.edu
    User alex
    IdentityFile ~/.ssh/id_ed25519
```

If you don't have a key on the NAS yet, the usual way is `ssh-keygen` to
create one and `ssh-copy-id nas` to install it; some NAS setups need an
administrator to add it instead. Then connect once by hand with `sftp nas`,
which also records the NAS's host key in `known_hosts`. In batch mode, ssh
won't stop to ask whether to trust a new host.

A key protected by a passphrase only works where an ssh-agent holds it, and
Slurm tasks usually don't have one. The check further down tells you whether
the key works from a compute node.

## Add the destination

Destinations go in your stomp config as `[[destination]]` blocks. Alex's has
one for the NAS:

```toml
[[destination]]
name = "nas"
type = "sftp"
host = "nas"              # anything `sftp` accepts, including a ~/.ssh/config alias
path = "/archive/$USER"   # where archives go on the NAS
```

- `name` is what you type after `--to`. `local` is reserved for archives kept
  inside their directory.
- `host` is passed to `sftp` and `ssh` as it is: a host alias, a host name, or
  `user@host`.
- `path` is the directory on the NAS. A path that starts with `~/`, or doesn't
  start with `/`, is relative to your home directory on the NAS. `$USER` and
  other variables are filled in on the cluster before connecting.
- `verify` chooses how each upload is checked. Leave it out, and stomp runs
  `sha256sum` on the NAS over SSH, or, if the NAS allows SFTP but not
  commands, downloads the file again and checks it locally. Set
  `"remote-sha256"` to insist on the first, `"readback"` to always download
  again, or `"size"` to compare sizes only (weak, so you must ask for it).
- There is no `parallel_uploads` for SFTP: each file goes up in one stream,
  one at a time. If you set it, stomp warns that it has no effect.

Every archive goes into its own directory, named by grant and archive ID:

```text
/archive/alex/USDA-1180/USDA-1180-20261118-e8c333/
    chunk-0001.tar.zst
    manifest.jsonl.zst
    README.txt
    SHA256SUMS
```

## Check it with stomp doctor

`stomp doctor` logs in to every destination, writes a small test file,
checks how uploads can be verified, and removes the file again:

```sh
stomp doctor
```

```text
ok    config       ~/.config/stomp/config.toml: 3 grant(s), 1 root(s), 1 destination(s)
…
ok    nas (sftp)   can log in to nas over SFTP
ok    nas (sftp)   can write to /archive/alex
ok    nas (sftp)   can run sha256sum over SSH, so uploads are verified on the server
info  destinations checked from this node; `stomp doctor --submit` checks from a compute node
…

No problems found.
```

The three `nas (sftp)` lines are the ones to read:

- If the login fails, stomp says `can't log in to nas without a password
  prompt`, followed by what `sftp` reported, and suggests checking that `sftp
  nas` works with an SSH key.
- If the directory can't be written, it says `can't write to /archive/alex`
  and why. stomp creates missing directories under `path` as long as it is
  allowed to.
- On a NAS that allows SFTP but not commands, the third line instead says `the
  server allows SFTP only, so uploads are verified by reading them back (twice
  the transfer)`. That works, but each chunk crosses the network twice. With
  `verify = "remote-sha256"` set, the same situation is a failure.

`stomp doctor --offline` skips the destinations, for when you only want the
other checks.

## Check it from a compute node

Your archives are made by Slurm tasks on compute nodes, and those may not reach
the same places a login node can. `--submit` runs the same checks as a Slurm
task. It asks for 1 CPU, 1 GB and 10 minutes, with the partition and account
from your `[slurm]` settings:

```sh
stomp doctor --submit
```

```text
Submitted the checks as Slurm job 4810233. When it finishes, `stomp tasks show doctor-20261118-180546-f577` points to the output.
```

```sh
stomp tasks show doctor-20261118-180546-f577
```

```text
Task      doctor-20261118-180546-f577
Kind      doctor
Status    done
…
Message   all checks passed
Script    ~/.local/share/stomp/tasks/doctor-20261118-180546-f577/task.sh
Log       ~/.local/share/stomp/tasks/doctor-20261118-180546-f577/slurm-4810233.out
```

The log holds the full report, as seen from the compute node:

```sh
cat ~/.local/share/stomp/tasks/doctor-20261118-180546-f577/slurm-4810233.out
```

```text
…
info  slurm        running inside job 4810233
info  threads      1 worker thread(s)
ok    nas (sftp)   can log in to nas over SFTP
ok    nas (sftp)   can write to /archive/alex
ok    nas (sftp)   can run sha256sum over SSH, so uploads are verified on the server
…

No problems found.
```

If the task's status is `failed`, its message says `some checks failed; see
the log`. When the NAS checks pass on a login node but fail here, compute nodes
can't reach the NAS; see [When compute nodes can't reach the
NAS](@/tutorials/nas-and-spaces.md#when-compute-nodes-can-t-reach-the-nas) below.

## Archive straight to the NAS

The marsh mapping grant ended in June. Its classified tiles should be kept, but
not on the cluster. Estimate first, as in the
[previous tutorial](@/tutorials/archive.md):

```sh
stomp archive ~/marsh_mapping/classified_tiles --estimate
```

```text
stomp: planning lists every file under ~/marsh_mapping/classified_tiles; on a big tree, run this inside a Slurm job.
Plan for archiving ~/marsh_mapping/classified_tiles (grant USDA-1180)

  files         16   25.7 MiB
  directories    0
  symlinks       0
  hard links     0
  chunks         1   up to 50.0 GiB each, zstd level 3

Estimate, from compressing 8 sampled files:
  compressed size   about 10.8 MiB (42% of the original), saving about 14.9 MiB
  files             16 → 7
  compression time  about 0s at 210 MB/s per thread with 8 thread(s); reading the files may take longer
```

With a remote destination, the savings on the cluster are the whole 25.7 MiB:
only a few small records stay behind. Archive it with `--to nas`:

```sh
stomp archive ~/marsh_mapping/classified_tiles --to nas
```

```text
Archive ~/marsh_mapping/classified_tiles (grant USDA-1180):
  packed into chunks of up to 50.0 GiB with zstd level 3, sent to
  nas:/archive/alex/USDA-1180/USDA-1180-20261118-e8c333
  (the archive's records stay in ~/marsh_mapping/classified_tiles/.stomp-archive/USDA-1180-20261118-e8c333)
Each chunk is verified at nas before the originals in it are removed.
Software environments, caches and files owned by others stay in place.
`--dry-run` shows the full plan; `--estimate` adds the expected size.
Archive it? [y/N] y
Submitted archive task archive-20261118-180548-1b93 as Slurm job 4810234. Follow it with `stomp tasks`.
Archive ID: USDA-1180-20261118-e8c333
```

The chunks go to the NAS. The archive's records, the small files that describe
it, stay in the directory.

For each chunk, the task:

1. writes it, then reads it back, decompresses it and checks every file, as
   for a local archive;
2. uploads it to the NAS under a `.partial` name, verifies it there, and only
   then gives it its real name;
3. deletes its local copy of the chunk;
4. removes the originals that went into it.

The manifest, `SHA256SUMS` and `README.txt` are uploaded last, after every
chunk. An archive directory on the NAS without a manifest is unfinished, so an
interrupted upload never looks complete. If the task stops partway, `stomp
archive --resume` continues it; a half-uploaded `.partial` file is simply sent
again.

```sh
stomp tasks show archive-20261118-180548-1b93
```

```text
…
Message   Archived ~/marsh_mapping/classified_tiles: 16 files (25.7 MiB) compressed to 10.7 MiB in 1 chunk, in 0s.
…
```

## See what stays on the cluster

```sh
cd ~/marsh_mapping/classified_tiles
ls -A
cat STOMP_ARCHIVED.txt
```

```text
.stomp-archive
STOMP_ARCHIVED.txt
This directory was archived by stomp on 2026-11-18 18:05 EST by alex.

Archive:   USDA-1180-20261118-e8c333
Location:  nas:/archive/alex/USDA-1180/USDA-1180-20261118-e8c333
           (records in .stomp-archive/USDA-1180-20261118-e8c333, inside this directory)
Contents:  16 files (25.7 MiB), compressed to 10.7 MiB in 1 chunk(s)

The originals were packed into compressed tar files and removed.

To restore with stomp:

    stomp restore USDA-1180-20261118-e8c333

To restore without stomp, see .stomp-archive/USDA-1180-20261118-e8c333/README.txt.
```

The note gives the archive's location on the NAS. Its records stay in the
directory, without the chunk:

```sh
ls .stomp-archive/USDA-1180-20261118-e8c333
```

```text
journal.jsonl
manifest.jsonl.zst
README.txt
settings.json
SHA256SUMS
```

Because the records are here, listing archives, `stomp archives show` and
working out what a restore needs all work without reaching the NAS. The
README, both here and on the NAS, explains how to download the files and
restore them without stomp.

In `stomp archives`, the **WHERE** column names the destination:

```sh
stomp archives
```

```text
ARCHIVE                     STATE      ORIGINAL   COMPRESSED   FILES   CREATED            WHERE   DIRECTORY
USDA-1180-20261118-e8c333   complete   25.7 MiB     10.7 MiB      16   2026-11-18 18:05   nas     ~/marsh_mapping/classified_tiles
```

`stomp archives show` gives both places, the records and the chunks:

```sh
stomp archives show USDA-1180-20261118-e8c333
```

```text
Archive    USDA-1180-20261118-e8c333
Directory  ~/marsh_mapping/classified_tiles
Records    ~/marsh_mapping/classified_tiles/.stomp-archive/USDA-1180-20261118-e8c333
Chunks     nas:/archive/alex/USDA-1180/USDA-1180-20261118-e8c333
Grant      USDA-1180
Created    2026-11-18 18:05 by alex on cn0412
Finished   2026-11-18 18:05
Contents   16 files (25.7 MiB), 0 directories, 0 symlinks, 0 hard links
Stored as  1 chunk(s), 10.7 MiB (42% of the original), zstd level 3
```

`stomp usage` lists what you have archived elsewhere, by destination and
grant, below its usual table:

```sh
stomp usage
```

```text
…
Archived elsewhere

DESTINATION   GRANT       ARCHIVES   ORIGINAL     STORED
nas           USDA-1180          1   25.7 MiB   10.7 MiB
```

## Check the copy on the NAS

`stomp archives verify` checks each chunk where it is stored. For this NAS,
that means running `sha256sum` there over SSH, so nothing is downloaded:

```sh
stomp archives verify USDA-1180-20261118-e8c333
```

```text
ok    chunk-0001.tar.zst
ok    manifest.jsonl.zst

All checks passed.
```

`--deep` downloads each chunk, decompresses it and checks every file, then
deletes the download. It costs a full transfer, so save it for when you need
proof that every file can come back:

```sh
stomp archives verify USDA-1180-20261118-e8c333 --deep
```

```text
ok    chunk-0001.tar.zst
ok    manifest.jsonl.zst

All checks passed, including every file's checksum.
```

## Restore from the NAS

Restoring works exactly as for a local archive. stomp downloads only the
chunks that hold the files you ask for, checks each one, extracts it and
deletes the download. A dry run says how many chunks it would download:

```sh
stomp restore USDA-1180-20261118-e8c333 --only 'tile_03.*' --dry-run
```

```text
Restoring archive USDA-1180-20261118-e8c333 into ~/marsh_mapping/classified_tiles:
  files                2   3.2 MiB
  directories          0
  links                0
  chunks               1   (1 to download from nas)
Nothing was changed (--dry-run).
```

```sh
stomp restore USDA-1180-20261118-e8c333 --only 'tile_03.*'
```

```text
Submitted restore task restore-20261118-180550-9dda as Slurm job 4810235. Follow it with `stomp tasks`.
```

```sh
stomp tasks show restore-20261118-180550-9dda
```

```text
…
Message   Restored 2 files (3.2 MiB), 0 links and 0 directories into ~/marsh_mapping/classified_tiles.
The archive was kept.
…
```

For a full restore, the dry run also says what happens to the copy on the NAS:

```sh
stomp restore USDA-1180-20261118-e8c333 --dry-run
```

```text
Restoring archive USDA-1180-20261118-e8c333 into ~/marsh_mapping/classified_tiles:
  files               16   25.7 MiB   (2 already in place)
  directories          0
  links                0
  chunks               1   (1 to download from nas)
Afterwards the archive is removed here; its copy at nas stays (--delete-remote deletes it).
Nothing was changed (--dry-run).
```

```sh
stomp restore USDA-1180-20261118-e8c333
```

```text
Submitted restore task restore-20261118-180551-7438 as Slurm job 4810236. Follow it with `stomp tasks`.
```

```sh
stomp tasks show restore-20261118-180551-7438
stomp archives
```

```text
…
Message   Restored 16 files (25.7 MiB), 0 links and 0 directories into ~/marsh_mapping/classified_tiles.
Everything is back. The copy at nas:/archive/alex/USDA-1180/USDA-1180-20261118-e8c333 is kept; `stomp archives delete USDA-1180-20261118-e8c333` deletes it.
…
ARCHIVE                     STATE                ORIGINAL   COMPRESSED   FILES   CREATED            WHERE   DIRECTORY
USDA-1180-20261118-e8c333   restored-copy-kept   25.7 MiB     10.7 MiB      16   2026-11-18 18:05   nas     ~/marsh_mapping/classified_tiles
```

The files are back and the directory is an ordinary directory again: the note
and `.stomp-archive` are gone. The copy on the NAS is kept as a backup, and
stomp keeps its records in `~/.local/share/stomp/manifests/<ID>`, so you can
still look at it, check it and restore from it:

```sh
stomp archives show USDA-1180-20261118-e8c333
```

```text
Archive    USDA-1180-20261118-e8c333
Directory  ~/marsh_mapping/classified_tiles
Records    ~/.local/share/stomp/manifests/USDA-1180-20261118-e8c333
Chunks     nas:/archive/alex/USDA-1180/USDA-1180-20261118-e8c333
State      the files were restored; this copy is kept (`stomp archives delete USDA-1180-20261118-e8c333` deletes it)
Grant      USDA-1180
Created    2026-11-18 18:05 by alex on cn0412
Finished   2026-11-18 18:05
Contents   16 files (25.7 MiB), 0 directories, 0 symlinks, 0 hard links
Stored as  1 chunk(s), 10.7 MiB (42% of the original), zstd level 3
```

stomp keeps the manifest there only if it is 64 MiB or smaller; a bigger one is
fetched from the NAS when it's needed. `stomp archives verify` works as before,
and so does `stomp restore`, with the ID or the directory. Here Alex pulls one
tile out of the backup into scratch space:

```sh
stomp restore USDA-1180-20261118-e8c333 --only 'tile_05.*' --to ~/scratch/tile05
```

```text
Submitted restore task restore-20261118-180552-8790 as Slurm job 4810237. Follow it with `stomp tasks`.
```

```sh
stomp tasks show restore-20261118-180552-8790
```

```text
…
Message   Restored 2 files (3.2 MiB), 0 links and 0 directories into ~/scratch/tile05.
The copy at nas:/archive/alex/USDA-1180/USDA-1180-20261118-e8c333 is kept.
…
```

When you no longer want the backup, `stomp archives delete` removes it from the
NAS. It asks first, since this can't be undone:

```sh
stomp archives delete USDA-1180-20261118-e8c333
```

```text
Delete archive USDA-1180-20261118-e8c333's copy at nas:/archive/alex/USDA-1180/USDA-1180-20261118-e8c333?
Its files were restored to ~/marsh_mapping/classified_tiles. Check they're still there if you need them: this can't be undone.
Delete it? [y/N] y
Deleted archive USDA-1180-20261118-e8c333's copy at nas:/archive/alex/USDA-1180/USDA-1180-20261118-e8c333.
```

The archive is then listed as `restored`, with nothing stored anywhere. If you
know at restore time that you don't want a copy left behind, add
`--delete-remote` to the full restore instead; there is an example
[further down](@/tutorials/nas-and-spaces.md#when-compute-nodes-can-t-reach-the-nas).

## Make the NAS the default

If most of your archives should go to the NAS, say so in the config:

```toml
[archive]
default_destination = "nas"
```

Then `stomp archive DIR` sends chunks to the NAS without `--to`, and `--to
local` keeps an archive inside its directory instead. Policies that don't name
a destination use this default too (see
[Automate archiving with policies](@/tutorials/automate.md)).

## Move an existing archive

An archive made locally can go to the NAS later, without compressing anything
again. Alex archived the 2024 calibration runs locally (archive
`NSF-2231-20261118-694e4a`) and now moves them:

```sh
stomp archives move NSF-2231-20261118-694e4a --to nas
```

```text
Move archive NSF-2231-20261118-694e4a from local to nas. Each chunk is verified at nas before any copy is removed.
Move it? [y/N] y
Submitted move task move-20261118-180556-eba2 as Slurm job 4810239. Follow it with `stomp tasks`.
If compute nodes can't reach nas, cancel it and run this with --here on a login node.
```

```sh
stomp tasks show move-20261118-180556-eba2
```

```text
…
Message   Moved archive NSF-2231-20261118-694e4a to nas:/archive/alex/NSF-2231/NSF-2231-20261118-694e4a.
…
```

Each chunk is uploaded and verified at the NAS before the local copy is
deleted. The records go up next, and the note and README are rewritten to say
where the archive now is. If a move stops partway, run the same command again
to continue it.

`--to local` brings the chunks back into the directory, and the NAS copy is
removed once they have all arrived and been checked:

```sh
stomp archives move NSF-2231-20261118-694e4a --to local
```

```text
Move archive NSF-2231-20261118-694e4a from nas to its directory. Each chunk is verified in the directory before any copy is removed.
Move it? [y/N] y
Submitted move task move-20261118-181730-9117 as Slurm job 4810240. Follow it with `stomp tasks`.
If compute nodes can't reach nas, cancel it and run this with --here on a login node.
```

```sh
stomp tasks show move-20261118-181730-9117
```

```text
…
Message   Moved archive NSF-2231-20261118-694e4a to ~/soil_carbon_runs/2024_calibration/.stomp-archive/NSF-2231-20261118-694e4a.
…
```

You can also move an archive from one destination to another, such as from
the NAS to Spaces. The chunks pass through the cluster: each is downloaded,
checked, uploaded and verified before the old copy is deleted.

## Build chunks somewhere else

By default, the chunks of a remote archive are written inside the directory
being archived, uploaded, and deleted one at a time. If your home quota is
tight, point `general.staging_dir` at scratch space instead:

```toml
[general]
roots       = ["~"]
staging_dir = "/scratch/$USER/stomp"
```

stomp then writes each chunk under `staging_dir`, in a directory named after
the archive, and also downloads chunks there for restores, moves and deep
verification. It never scans, proposes or archives the staging directory
itself. Archives kept locally don't use it: their chunks belong in the
directory.

On CephFS with quotas, stomp checks before it starts that there's room for the
chunk being written where it will be written: under `staging_dir` if it is set,
otherwise in the directory being archived. If a remote archive doesn't fit and
`staging_dir` isn't set, the error suggests setting it.

## When compute nodes can't reach the NAS

On many clusters only login or data-transfer nodes can reach outside machines.
If `stomp doctor --submit` shows the NAS failing from a compute node, split the
work in two. First archive locally in a task, which does the compressing on a
compute node:

```sh
stomp archive ~/marsh_mapping/field_photos --to local --yes
```

```text
Submitted archive task archive-20261118-180600-9ae6 as Slurm job 4810241. Follow it with `stomp tasks`.
Archive ID: USDA-1180-20261118-9f52e9
```

When that task is done, move the archive from a login or data-transfer node
with `--here`. Moving mostly waits on the network, not the CPU:

```sh
stomp archives move USDA-1180-20261118-9f52e9 --to nas --here
```

```text
Move archive USDA-1180-20261118-9f52e9 from local to nas. Each chunk is verified at nas before any copy is removed.
Move it? [y/N] y
Moved archive USDA-1180-20261118-9f52e9 to nas:/archive/alex/USDA-1180/USDA-1180-20261118-9f52e9.
```

Restores work the same way: if a restore task can't download from the NAS, run
`stomp restore` with `--here` on a node that can.

Months later, Alex needs the photos back for good and doesn't want a copy left
on the NAS. `--delete-remote` deletes it once everything is back:

```sh
stomp restore USDA-1180-20261118-9f52e9 --delete-remote --dry-run
```

```text
Restoring archive USDA-1180-20261118-9f52e9 into ~/marsh_mapping/field_photos:
  files              400   800.0 KiB
  directories          0
  links                0
  chunks               1   (1 to download from nas)
Afterwards the archive is removed, and so is its copy at nas.
Nothing was changed (--dry-run).
```

```sh
stomp restore USDA-1180-20261118-9f52e9 --delete-remote
```

```text
Submitted restore task restore-20261118-180602-17a9 as Slurm job 4810242. Follow it with `stomp tasks`.
```

```sh
stomp tasks show restore-20261118-180602-17a9
```

```text
…
Message   Restored 400 files (800.0 KiB), 0 links and 0 directories into ~/marsh_mapping/field_photos.
Everything is back, so the archive was removed.
…
```

When stomp deletes an archive at a destination, here or when moving, undoing
or running `stomp archives delete`, it removes the manifest first. A
half-deleted archive therefore never looks complete.

## Add DigitalOcean Spaces

Spaces, and other S3-compatible services such as AWS S3, MinIO, Wasabi or a
campus object store, are a second kind of destination. stomp has its own small
S3 client built in, so nothing else needs to be installed.

Create the bucket and an access key in the DigitalOcean control panel first;
stomp doesn't create buckets. Then add a destination:

```toml
[[destination]]
name     = "spaces"
type     = "s3"
endpoint = "https://nyc3.digitaloceanspaces.com"
bucket   = "rivera-lab-archive"
prefix   = "stomp/$USER"
profile  = "spaces"
```

- `endpoint` is the service's URL, without a path. For Spaces it is
  `https://<region>.digitaloceanspaces.com`. Leave it out for AWS, and stomp
  uses `https://s3.<region>.amazonaws.com`.
- `bucket` must already exist.
- `prefix` is an optional folder inside the bucket. Archives go under
  `<prefix>/<grant>/<archive ID>/`.
- `region` defaults to `us-east-1`. Spaces ignores it, but requests are signed
  with it, so set it to the bucket's region on AWS.
- `profile` names the credentials to use; see below.
- `force_path_style = true` puts the bucket in the URL path
  (`https://host/bucket/...`) instead of the host name
  (`https://bucket.host/...`). Spaces and AWS don't need it; some other
  services do.
- `storage_class` can only be `"STANDARD"`, the default, so leave it out. Any
  other class is a config error: classes such as AWS's `GLACIER` must be thawed
  before a restore, which stomp doesn't do yet.
- `verify` is `"server-checksum"` (the default), `"readback"` or `"size"`;
  see below.
- `parallel_uploads` is how many parts of a large chunk are uploaded at once,
  4 by default and at most 16. Spaces limits request rates, so keep it modest.

### Credentials

stomp reads the same files as the AWS tools. With `profile = "spaces"`, put
the Spaces key in `~/.aws/credentials`, readable only by you:

```text
[spaces]
aws_access_key_id     = DO00EXAMPLEKEY
aws_secret_access_key = example-secret
```

stomp looks for `aws_access_key_id` and `aws_secret_access_key` (and
`aws_session_token`, if present) under `[spaces]` in `~/.aws/credentials`, then
under `[profile spaces]` in `~/.aws/config`. `AWS_SHARED_CREDENTIALS_FILE` and
`AWS_CONFIG_FILE` point it at other files.

Without a `profile` in the destination, stomp uses `AWS_ACCESS_KEY_ID` and
`AWS_SECRET_ACCESS_KEY` from the environment if both are set, and otherwise
the profile named by `AWS_PROFILE`, or `default`. A profile in the credentials
file is the dependable choice: archive tasks and the daily run read the same
file, while environment variables only help if they reach the job. stomp reads
keys only; it doesn't run SSO logins or credential helpers. Spaces keys don't
expire, so they suit long tasks.

Until the key is in place, `stomp doctor` says what is missing:

```sh
stomp doctor
```

```text
…
FAIL  spaces (s3)  destination spaces: no S3 credentials for profile "spaces": add aws_access_key_id and aws_secret_access_key under [spaces] in ~/.aws/credentials
…
```

Archiving with `--to spaces` stops with the same message. `--dry-run` and
`--estimate` don't contact the destination, so they work before the key is set
up.

### What stomp doctor checks for S3

With credentials in place, `stomp doctor` connects to the bucket and works
through these checks, printing a line for each:

1. It reports the key it found: `credentials found (access key DO00EX…)`,
   with only the first six characters shown.
2. It lists the bucket: `bucket rivera-lab-archive is reachable at
   nyc3.digitaloceanspaces.com`, or `can't reach bucket ... at ...` with the
   reason.
3. It uploads a small test object under the prefix and verifies it: `can
   upload and verify objects`, or `can't upload a test object` with the
   reason.
4. It uploads the test object twice more with a deliberately wrong checksum,
   once a wrong Content-MD5 and once a wrong SHA-256, and expects the server to
   reject both. If it does: `the server rejects uploads whose Content-MD5 or
   SHA-256 doesn't match`. If the server accepts both while `verify` is
   `"server-checksum"`, that is a failure: `the server accepted uploads with
   wrong checksums; set verify = "readback" for this destination`.

It then deletes the test object. Run `stomp doctor --submit` as well:
Spaces is on the public internet, and compute nodes often can't reach it. If
they can't, use the two-step approach from the NAS section, with `--to spaces`.

### How uploads are checked

Every upload stomp sends to S3 carries the SHA-256 of its contents in its
signature, and a Content-MD5 header as well, so the server rejects anything
damaged on the way. Each chunk is also stored with its own SHA-256 as object
metadata. After an upload, `verify` decides what else is checked:

- `"server-checksum"` (the default) checks the stored object's size and the
  SHA-256 it carries. This relies on the server checking what it receives,
  which is what `stomp doctor` tests.
- `"readback"` downloads the object again and hashes it. Spaces bills outbound
  transfer beyond your plan's allowance, so this costs money as well as time.
- `"size"` compares sizes only.

A chunk no larger than 64 MiB goes up in one request. Larger chunks are sent
in parts, `parallel_uploads` at a time. If the task is interrupted, the resumed
archive asks the server which parts it already has and sends only the rest.
Network errors, throttling and server errors are retried with a growing pause.

When stomp deletes an archive from the bucket, after a move, with
`--delete-remote`, when undoing an unfinished archive or with `stomp archives
delete`, it also aborts any unfinished multipart uploads of that archive. An
upload that is interrupted and then never resumed or undone can still leave
parts in the bucket, so a lifecycle rule that aborts incomplete multipart
uploads after a few days is a sensible backstop.

Once `stomp doctor` is happy, everything in this tutorial works with
`--to spaces` in place of `--to nas`: archiving, `stomp archives move`,
`stomp archives verify` and restoring.

## What's next

- [Automate archiving with policies](@/tutorials/automate.md) to have stomp
  propose directories to send to the NAS.
- [Destinations](@/concepts/destinations.md) explains how each kind of
  destination stores and verifies archives.
- The [`[[destination]]` reference](@/reference/configuration.md#destination)
  lists every setting.
- [Troubleshooting](@/reference/troubleshooting.md) for when something goes
  wrong.
