+++
title = "Archive a finished project"
description = "Estimate, archive, verify and restore a directory, all inside your own storage."
weight = 60

[extra]
time = "25 minutes"
needs = "a working config with your grants (see the first tutorial)"
+++

Alex's 2024 calibration runs for the soil carbon model are finished and the
paper is out. In this tutorial you pack `~/soil_carbon_runs/2024_calibration`
into a compressed, verified archive that stays inside the directory, check it,
restore part of it, and then put everything back. Along the way you'll see what
stomp leaves in place, how to choose a compression level and chunk size, and
how to continue an archive that stopped partway.

The directory holds six run directories, `run_00` to `run_05`, each with four
CSV files of model output, a `params.yaml` and a `model.log`: 36 files, 2.4 MiB.
Your own directories will be much bigger, but the steps are the same.

## Look at the plan

Start with `--dry-run`. It lists everything that would be archived and changes
nothing:

```sh
stomp archive ~/soil_carbon_runs/2024_calibration --dry-run
```

```text
stomp: planning lists every file under ~/soil_carbon_runs/2024_calibration; on a big tree, run this inside a Slurm job.
Plan for archiving ~/soil_carbon_runs/2024_calibration (grant NSF-2231)

  files         36   2.4 MiB
  directories    6
  symlinks       0
  hard links     0
  chunks         1   up to 50.0 GiB each, zstd level 3
```

The first line is a reminder that planning walks the whole tree. For a few
thousand files that takes a moment; for millions, run it from inside a Slurm
job, such as an interactive session, rather than on a login node.

The plan shows:

- **files**: how many regular files would be packed, and their total size.
- **directories**, **symlinks**, **hard links**: these are archived too.
  Symlinks are stored as links and never followed. Hard links are kept as
  links within the archive.
- **chunks**: how many compressed files the archive will be split into. Files
  are packed in name order into chunks of up to `archive.chunk_size` (50 GiB
  unless you change it) and compressed with zstd at `archive.zstd_level` (3
  unless you change it). A file is never split across chunks.

stomp also records the grant the directory belongs to, here `NSF-2231`. The
archive stays in the directory, so its space still counts toward that grant.

## Estimate the result

`--estimate` shows the same plan and then compresses a sample of the files to
predict the result. It changes nothing either:

```sh
stomp archive ~/soil_carbon_runs/2024_calibration --estimate
```

```text
stomp: planning lists every file under ~/soil_carbon_runs/2024_calibration; on a big tree, run this inside a Slurm job.
Plan for archiving ~/soil_carbon_runs/2024_calibration (grant NSF-2231)

  files         36   2.4 MiB
  directories    6
  symlinks       0
  hard links     0
  chunks         1   up to 50.0 GiB each, zstd level 3

Estimate, from compressing 24 sampled files:
  compressed size   about 871.5 KiB (35% of the original), saving about 1.6 MiB
  files             36 → 7
  compression time  about 0s at 194 MB/s per thread with 8 thread(s); reading the files may take longer
```

- **compressed size**: stomp compressed the start of up to 64 files, picking
  big files more often than small ones, and scaled the result up. CSV and log
  files compress well; data that is already compressed (JPEG, most GeoTIFFs and
  NetCDF-4 files) barely shrinks.
- **files**: the number of files before and after. After archiving, the
  directory holds the chunks plus six small files: the note, the manifest, the
  checksums, a README, a journal and the archive's settings. On a cluster that
  limits how many files you may have, this can matter more than the bytes.
- **compression time**: the CPU time alone, spread over the task's threads.
  Reading the files from disk often takes longer.

## Archive it

Now archive it for real. stomp shows what it is about to do and asks first:

```sh
stomp archive ~/soil_carbon_runs/2024_calibration
```

```text
Archive ~/soil_carbon_runs/2024_calibration (grant NSF-2231):
  packed into chunks of up to 50.0 GiB with zstd level 3, kept in
  ~/soil_carbon_runs/2024_calibration/.stomp-archive/NSF-2231-20261118-200375
Each chunk is read back and checked before the originals in it are removed.
Software environments, caches and files owned by others stay in place.
`--dry-run` shows the full plan; `--estimate` adds the expected size.
Archive it? [y/N] y
Submitted archive task archive-20261118-180433-5340 as Slurm job 4810233. Follow it with `stomp tasks`.
Archive ID: NSF-2231-20261118-200375
```

The work runs as a Slurm task, using the `[slurm]` settings in your config.
Override them for one archive with `--time`, `--cpus`, `--mem`, `--partition`
and `--account`. Planning keeps the whole file list in memory, about 150 bytes
per file, so a tree of 10 million files needs about 1.5 GB of `--mem` for that
alone.

The archive ID is the grant, the date and six random characters. A directory
that belongs to no grant gets `unassigned` instead of a grant.

`--yes` (or `-y`) skips the question. You need it in scripts: without a
terminal to ask on, stomp stops with `this needs confirmation; run it again
with --yes`.

## Follow the task

```sh
stomp tasks
```

```text
TASK                           STATUS    SLURM JOB   UPDATED            PROGRESS
archive-20261118-180433-5340   running   4810233     2026-11-18 18:04
```

The **STATUS** is `submitted` while the job waits in the queue, then `running`,
and finally `done`, `failed`, `incomplete` (it stopped early; see below),
`partial` (a restore that had to leave some files alone) or `cancelled`. **PROGRESS** fills in as the task works; it is updated every ten
seconds. Run it again once the job has finished:

```sh
stomp tasks
```

```text
TASK                           STATUS   SLURM JOB   UPDATED            PROGRESS
archive-20261118-180433-5340   done     4810233     2026-11-18 18:04   36 files, 2.4 MiB
```

`stomp tasks show` has the outcome and where the job script and its log are:

```sh
stomp tasks show archive-20261118-180433-5340
```

```text
Task      archive-20261118-180433-5340
Kind      archive
Status    done
Created   2026-11-18 18:04
Updated   2026-11-18 18:04
Jobs      4810233
Progress  36 files, 2.4 MiB
Message   Archived ~/soil_carbon_runs/2024_calibration: 36 files (2.4 MiB) compressed to 820.3 KiB in 1 chunk, in 0s.
Script    ~/.local/share/stomp/tasks/archive-20261118-180433-5340/task.sh
Log       ~/.local/share/stomp/tasks/archive-20261118-180433-5340/slurm-4810233.out
```

For each chunk, the task wrote it, read it back, decompressed it and compared
every file's checksum with the one it recorded while packing. Only then did it
remove that chunk's originals, and only those that hadn't changed since they
were packed (same size, modification time, change time and inode). Directories
are removed at the very end, and only once they are empty.

## See what's left in the directory

```sh
cd ~/soil_carbon_runs/2024_calibration
ls -A
```

```text
.stomp-archive
STOMP_ARCHIVED.txt
```

`STOMP_ARCHIVED.txt` is for anyone who opens the directory later, including
colleagues who have never heard of stomp:

```sh
cat STOMP_ARCHIVED.txt
```

```text
This directory was archived by stomp on 2026-11-18 18:04 EST by alex.

Archive:   NSF-2231-20261118-200375
Location:  .stomp-archive/NSF-2231-20261118-200375 (inside this directory)
Contents:  36 files (2.4 MiB), compressed to 820.3 KiB in 1 chunk(s)

The originals were packed into compressed tar files and removed.

To restore with stomp:

    stomp restore NSF-2231-20261118-200375

To restore without stomp, see .stomp-archive/NSF-2231-20261118-200375/README.txt.
```

The archive itself is in `.stomp-archive/<ID>`:

```sh
ls .stomp-archive/NSF-2231-20261118-200375
```

```text
chunk-0001.tar.zst
journal.jsonl
manifest.jsonl.zst
README.txt
settings.json
SHA256SUMS
```

- `chunk-0001.tar.zst` is an ordinary tar file compressed with zstd. Each chunk
  can be extracted on its own.
- `manifest.jsonl.zst` lists every file with its size, permissions, times,
  owner, chunk and checksum, plus anything that was left in place. Read it with
  `zstdcat manifest.jsonl.zst | jq`.
- `SHA256SUMS` holds the checksums of the chunks and the manifest, in the
  format `sha256sum -c` checks.
- `README.txt` explains how to restore with stomp, and without it using only
  `sha256sum`, `zstd` and `tar`, with this directory's paths filled in. The
  archive never depends on stomp being installed.
- `journal.jsonl` records every step, so an interrupted archive can continue.
- `settings.json` holds the settings the archive was made with.

The [archive format](@/reference/archive-format.md) reference describes these
files in detail.

## Know what stays in place

Some things are never packed, even inside a directory you archive:

- **software environments**: conda environments, Python virtual environments
  and Apptainer sandboxes, recognized by their `conda-meta`, `pyvenv.cfg` or
  `.singularity.d`;
- **caches**: `~/.cache`, `~/.apptainer/cache`, `~/.singularity/cache` and
  `~/.conda/pkgs`, and anything matching `scan.exclude` in your config;
- **files owned by other people**, such as a collaborator's files in a shared
  project directory (set `archive.only_owned = false` to change this);
- sockets, pipes and device files, other filesystems mounted inside the
  directory, files stomp can't read, and older stomp archives nested inside.

The calibration runs had none of these, so the plan had no "Left in place"
section. `~/soil_carbon` has a Python virtual environment, and its plan shows
it:

```sh
stomp archive ~/soil_carbon --dry-run
```

```text
stomp: planning lists every file under ~/soil_carbon; on a big tree, run this inside a Slurm job.
Plan for archiving ~/soil_carbon (grant NSF-2231)

  files         39   82.8 GiB
  directories    6
  symlinks       0
  hard links     0
  chunks         2   up to 50.0 GiB each, zstd level 3

Left in place:
  software environments                           1   70.3 KiB
```

Two more kinds of file stay where they are while archiving runs: a file that
changes while it is being packed or before its chunk is checked (it is never
deleted, and the manifest marks it), and a file created after planning, which
isn't in the plan at all.

stomp also refuses some directories outright: a root itself, a directory that
contains another grant's directories, an environment, a cache, and a directory
that is already archived. For example:

```sh
stomp archive ~ --dry-run
stomp archive ~/.cache --dry-run
stomp archive ~/soil_carbon/.venv --dry-run
```

```text
stomp: error: ~ is a root; archive the directories inside it instead
stomp: error: ~/.cache is excluded from scanning and archiving (cache)
stomp: error: ~/soil_carbon/.venv is a software environment; stomp doesn't archive these (a Python venv can be rebuilt from `~/soil_carbon/.venv/bin/pip freeze > requirements.txt`)
```

Environments are better kept as a recipe than as an archive: for a conda
environment, stomp suggests `conda env export`, and for an Apptainer sandbox,
building it into a single image file.

## List and check your archives

```sh
stomp archives
```

```text
ARCHIVE                    STATE      ORIGINAL   COMPRESSED   FILES   CREATED            WHERE       DIRECTORY
NSF-2231-20261118-200375   complete    2.4 MiB    820.3 KiB      36   2026-11-18 18:04   directory   ~/soil_carbon_runs/2024_calibration
```

- **STATE** is `in-progress`, `complete` or `restored`. An unfinished archive
  that was undone shows `abandoned`, and one restored from a remote destination
  whose copy was kept there shows `restored-copy-kept`.
- **ORIGINAL** and **COMPRESSED** are the sizes before and after, and **FILES**
  the number of files packed. For an archive still in progress, they are what
  was planned.
- **CREATED** is when the archive was made.
- **WHERE** is where the chunks are: `directory` for an archive kept inside its
  directory, the destination's name for one sent elsewhere, and `—` once
  nothing is stored any more.
- **DIRECTORY** is where the files came from.

`stomp archives show` gives the details of one archive. You can name it by its
ID or by the archived directory:

```sh
stomp archives show NSF-2231-20261118-200375
```

```text
Archive    NSF-2231-20261118-200375
Directory  ~/soil_carbon_runs/2024_calibration
Records    ~/soil_carbon_runs/2024_calibration/.stomp-archive/NSF-2231-20261118-200375
Chunks     inside the directory, with the records
Grant      NSF-2231
Created    2026-11-18 18:04 by alex on cn0412
Finished   2026-11-18 18:04
Contents   36 files (2.4 MiB), 6 directories, 0 symlinks, 0 hard links
Stored as  1 chunk(s), 820.3 KiB (33% of the original), zstd level 3
```

**Records** is where the manifest, checksums and journal are, and **Chunks**
where the compressed data is. For a local archive they are the same place. The
estimate said 35%; the real archive came out at 33%.

`stomp archives verify` checks each chunk and the manifest against
`SHA256SUMS`:

```sh
stomp archives verify NSF-2231-20261118-200375
```

```text
ok    chunk-0001.tar.zst
ok    manifest.jsonl.zst

All checks passed.
```

`--deep` also decompresses every chunk and checks every file's checksum. It
reads and decompresses the whole archive, so it takes much longer, but it shows
that every file can come back:

```sh
stomp archives verify NSF-2231-20261118-200375 --deep
```

```text
ok    chunk-0001.tar.zst
ok    manifest.jsonl.zst

All checks passed, including every file's checksum.
```

Both print `FAIL` and the reason for a chunk that doesn't check out, and exit
with a non-zero status.

## Restore part of it

Alex wants to compare the parameter files of all six runs without unpacking
the model output. `--only` takes a pattern relative to the archived directory,
and `--to` restores somewhere else instead of the original place. Add
`--dry-run` first to see what a restore would do:

```sh
stomp restore NSF-2231-20261118-200375 --only '*/params.yaml' --to ~/scratch/calibration_params --dry-run
```

```text
Restoring archive NSF-2231-20261118-200375 into ~/scratch/calibration_params:
  files                6   216 B
  directories          6
  links                0
  chunks               1
Nothing was changed (--dry-run).
```

The **directories** are the six run directories the files sit in, which the
restore recreates. The **chunks** line is how many chunks stomp has to read.
With a remote archive it also says how many it has to download.

A pattern has to match the whole path from the top of the archived directory,
so `params.yaml` on its own would only match a file at the top. stomp checks
the patterns before it submits anything:

```sh
stomp restore NSF-2231-20261118-200375 --only 'params.yaml'
```

```text
stomp: error: nothing in the archive matches --only "params.yaml"; patterns are relative to the archived directory, e.g. `results` or `results/*.csv`
```

In patterns, `*` matches within one directory level and `**` matches across
levels, and a pattern that names a directory includes everything in it. Quote
the pattern so your shell doesn't expand it, and repeat `--only` for several
patterns. Now restore for real:

```sh
stomp restore NSF-2231-20261118-200375 --only '*/params.yaml' --to ~/scratch/calibration_params
```

```text
Submitted restore task restore-20261118-180435-c8fc as Slurm job 4810234. Follow it with `stomp tasks`.
```

Like archiving, restoring runs as a Slurm task unless you add `--here`. The
`--to` directory is created when the task runs. When it has finished, `stomp
tasks show` has the result:

```sh
stomp tasks show restore-20261118-180435-c8fc
```

```text
Task      restore-20261118-180435-c8fc
Kind      restore
Status    done
…
Message   Restored 6 files (216 B), 0 links and 6 directories into ~/scratch/calibration_params.
The archive was kept.
…
```

The files keep their paths relative to the archived directory:

```sh
cd ~/scratch
find calibration_params -type f | sort
```

```text
calibration_params/run_00/params.yaml
calibration_params/run_01/params.yaml
calibration_params/run_02/params.yaml
calibration_params/run_03/params.yaml
calibration_params/run_04/params.yaml
calibration_params/run_05/params.yaml
```

stomp reads only the chunks that hold matching files. It checks each chunk's
checksum before extracting it and each file's checksum as it writes it. Files
get back their permissions and modification times, and belong to you.

Without `--to`, files go back to where they came from. Alex now wants run 3
back in place to rerun it:

```sh
stomp restore NSF-2231-20261118-200375 --only run_03
```

```text
Submitted restore task restore-20261118-180436-19ac as Slurm job 4810235. Follow it with `stomp tasks`.
```

Once it has finished, `run_03` sits next to the archive, which is kept because
only part of it was restored:

```sh
ls -A ~/soil_carbon_runs/2024_calibration
```

```text
run_03
.stomp-archive
STOMP_ARCHIVED.txt
```

## Restore everything

Alex changed `run_03/params.yaml` before rerunning it. Later the whole project
needs to come back. A dry run shows what is already there:

```sh
stomp restore NSF-2231-20261118-200375 --dry-run
```

```text
Restoring archive NSF-2231-20261118-200375 into ~/soil_carbon_runs/2024_calibration:
  files               36   2.4 MiB   (5 already in place)
  directories          6
  links                0
  chunks               1
1 existing file(s) differ from the archive and would be left alone (--overwrite replaces them): run_03/params.yaml
Nothing was changed (--dry-run).
```

stomp never replaces a file that already exists unless you ask. A file whose
size and modification time already match the archive counts as restored: those
are the five files of `run_03` "already in place". The changed `params.yaml` is
a conflict. Restoring anyway brings back everything else:

```sh
stomp restore NSF-2231-20261118-200375
```

```text
Submitted restore task restore-20261118-180437-6c06 as Slurm job 4810236. Follow it with `stomp tasks`.
```

```sh
stomp tasks
```

```text
TASK                           STATUS    SLURM JOB   UPDATED            PROGRESS
archive-20261118-180433-5340   done      4810233     2026-11-18 18:04   36 files, 2.4 MiB
restore-20261118-180435-c8fc   done      4810234     2026-11-18 18:04
restore-20261118-180436-19ac   done      4810235     2026-11-18 18:04
restore-20261118-180437-6c06   partial   4810236     2026-11-18 18:04
```

The task ends as `partial` because it left a file alone. Its message says
which:

```sh
stomp tasks show restore-20261118-180437-6c06
```

```text
…
Message   Restored 35 files (2.4 MiB), 0 links and 6 directories into ~/soil_carbon_runs/2024_calibration.
1 file(s) already existed and were left alone (use --overwrite to replace them): run_03/params.yaml
The archive was kept.
…
```

With a file left alone, the archive is kept. You have two ways forward: move the changed file aside and run the restore
again, or replace it with the archived copy using `--overwrite`. Alex's change
was only a test, so Alex overwrites it. stomp asks first:

```sh
stomp restore NSF-2231-20261118-200375 --overwrite
```

```text
Files that already exist where the archive restores to will be replaced.
Continue? [y/N] y
Submitted restore task restore-20261118-180439-c634 as Slurm job 4810237. Follow it with `stomp tasks`.
```

```sh
stomp tasks show restore-20261118-180439-c634
```

```text
…
Message   Restored 36 files (2.4 MiB), 0 links and 6 directories into ~/soil_carbon_runs/2024_calibration.
Everything is back, so the archive was removed.
…
```

After a complete restore of everything into the original place, with nothing
left alone, stomp removes the archive, its `.stomp-archive` directory and the
note. Add `--keep-archive` to keep it anyway. A restore with `--only` or `--to`
never removes the archive.

The directory looks as it did before, and the catalog remembers the archive:

```sh
ls -A ~/soil_carbon_runs/2024_calibration
stomp archives
```

```text
run_00
run_01
run_02
run_03
run_04
run_05
ARCHIVE                    STATE      ORIGINAL   COMPRESSED   FILES   CREATED            WHERE   DIRECTORY
NSF-2231-20261118-200375   restored    2.4 MiB    820.3 KiB      36   2026-11-18 18:04   —       ~/soil_carbon_runs/2024_calibration
```

## Choose a compression level and chunk size

Two options change how the archive is built:

- `--level N` is the zstd compression level, from 1 to 22. The default is
  `archive.zstd_level` in your config, or 3. Higher levels make smaller chunks
  but take much more CPU time. Try a level with `--estimate` before you commit.
- `--chunk-size SIZE`, such as `10GiB`, is the largest chunk. The default is
  `archive.chunk_size`, or 50 GiB, and the smallest allowed is 1 MiB. Smaller
  chunks mean a partial restore reads less, and archiving needs less free space
  while it works, since it builds one chunk at a time. Larger chunks mean fewer
  files. A file bigger than the chunk size gets a chunk of its own.

Both are saved with the archive. Here is the plan with tiny chunks and a high
level:

```sh
stomp archive ~/soil_carbon_runs/2024_calibration --dry-run --chunk-size 1MiB --level 19
```

```text
stomp: planning lists every file under ~/soil_carbon_runs/2024_calibration; on a big tree, run this inside a Slurm job.
Plan for archiving ~/soil_carbon_runs/2024_calibration (grant NSF-2231)

  files         36   2.4 MiB
  directories    6
  symlinks       0
  hard links     0
  chunks         3   up to 1.0 MiB each, zstd level 19
```

On CephFS with a quota, stomp checks before it starts that there is room under
the quota for the chunk it is writing. If there isn't, it stops and suggests
freeing some space or using a smaller `--chunk-size`.

## Continue an archive that stopped

A big archive can outlast its task's time limit. Slurm warns the task five
minutes before the limit; the task stops at the next step it can resume from
and, with `slurm.auto_resubmit` on (the default), submits itself again to carry
on, up to five times. After that, or with resubmitting turned off, the task
ends as `incomplete` and its message says how to continue.

You can also stop an archive yourself. Here Alex archives in the foreground
with `--here` (more on that below), using the settings from the plan above,
and presses Ctrl-C partway through:

```sh
stomp archive ~/soil_carbon_runs/2024_calibration --here --yes --chunk-size 1MiB --level 19
```

```text
stomp: archiving here instead of in a Slurm task; Ctrl-C stops it safely and `--resume` continues.
stopped after 1 of 3 chunk(s); everything removed so far is safe in the archive. Continue with `stomp archive --resume ~/soil_carbon_runs/2024_calibration`.
```

The first chunk was finished and its originals removed. `run_00`, which was
entirely in that chunk, is empty now, but empty directories aren't removed
until the end. Part of `run_02` went into the first chunk, and `run_05` hasn't
been touched yet:

```sh
cd ~/soil_carbon_runs/2024_calibration
ls run_00 run_02 run_05
```

```text
run_00:

run_02:
output_1.csv
output_2.csv
output_3.csv
params.yaml

run_05:
model.log
output_0.csv
output_1.csv
output_2.csv
output_3.csv
params.yaml
```

Everything that was removed is safe in the finished chunk. stomp knows the
archive is unfinished and won't start another one over it:

```sh
stomp archive ~/soil_carbon_runs/2024_calibration
```

```text
stomp: error: archiving ~/soil_carbon_runs/2024_calibration was started earlier (NSF-2231-20261118-2def23); continue it with `stomp archive --resume ~/soil_carbon_runs/2024_calibration`
```

The archive is listed as `in-progress`, with the size and file count it was
planned with:

```sh
stomp archives
```

```text
ARCHIVE                    STATE         ORIGINAL   COMPRESSED   FILES   CREATED            WHERE       DIRECTORY
NSF-2231-20261118-2def23   in-progress    2.4 MiB            —      36   2026-11-18 18:04   directory   ~/soil_carbon_runs/2024_calibration
NSF-2231-20261118-200375   restored       2.4 MiB    820.3 KiB      36   2026-11-18 18:04   —           ~/soil_carbon_runs/2024_calibration
```

`stomp archives show` reports how far it got, and `stomp doctor` warns about
it until it is finished or undone:

```sh
stomp archives show ~/soil_carbon_runs/2024_calibration
```

```text
Archive    NSF-2231-20261118-2def23
Directory  ~/soil_carbon_runs/2024_calibration
Records    ~/soil_carbon_runs/2024_calibration/.stomp-archive/NSF-2231-20261118-2def23
Chunks     inside the directory, with the records
State      unfinished: 1 of 3 chunk(s) done
Planned    36 files, 2.4 MiB
```

The note in the directory explains the situation to anyone who finds it:

```sh
cat STOMP_ARCHIVED.txt
```

```text
This directory is being archived by stomp (archive NSF-2231-20261118-2def23), started 2026-11-18 18:04 EST by alex.

Files are packed into compressed chunks and the originals are removed one
chunk at a time, after each chunk has been verified where it's stored. If
archiving stops partway, everything already removed is safe in those chunks.

Chunks:                 .stomp-archive/NSF-2231-20261118-2def23 (inside this directory)
To continue:            stomp archive --resume ~/soil_carbon_runs/2024_calibration
To put everything back: stomp restore ~/soil_carbon_runs/2024_calibration
```

You have two choices: finish the archive, or undo it. Undoing is a full
restore of the unfinished archive. A dry run shows what it would involve:

```sh
stomp restore ~/soil_carbon_runs/2024_calibration --dry-run
```

```text
Restoring archive NSF-2231-20261118-2def23 into ~/soil_carbon_runs/2024_calibration:
  files               27   1.8 MiB   (13 already in place)
  directories          5
  links                0
  chunks               2
The archive is unfinished: restoring everything undoes it, and what was archived so far is removed.
Nothing was changed (--dry-run).
```

Only the chunks written so far are involved. Files that weren't removed yet
are already in place. Once everything is back, stomp deletes the chunks, the
records and the note, and the archive is listed as `abandoned`. The directory
is then an ordinary directory again, and you can archive it afresh. If the
archive's task is still queued or running, stomp refuses and asks you to cancel
it first with `stomp tasks cancel`. Cancelling an archive task stops it the way
Ctrl-C does, and says what to do next:

```sh
stomp tasks cancel archive-20261118-181743-a0ce
```

```text
Cancelled task archive-20261118-181743-a0ce (Slurm job 4810239). Everything removed so far is safe in the archive; `stomp archive --resume ~/soil_carbon_runs/2024_calibration` continues it, and `stomp restore ~/soil_carbon_runs/2024_calibration` undoes it.
```

A cancelled task stays cancelled: only Slurm's warning before a time limit
makes a task submit itself again.

Alex would rather finish it. `--resume` asks, then submits a task:

```sh
stomp archive --resume ~/soil_carbon_runs/2024_calibration
```

```text
Continue archiving ~/soil_carbon_runs/2024_calibration (grant NSF-2231), archive NSF-2231-20261118-2def23.
Continue archiving? [y/N] y
Submitted archive task archive-20261118-180443-26bf as Slurm job 4810238. Follow it with `stomp tasks`.
Archive ID: NSF-2231-20261118-2def23
```

The resumed archive picks up where the journal says it stopped: a half-written
chunk is written again, an unchecked one is checked, and an interrupted
deletion is repeated. It keeps the level and chunk size it started with, so
you don't need to give them again. When the task is done:

```sh
stomp archives show ~/soil_carbon_runs/2024_calibration
```

```text
Archive    NSF-2231-20261118-2def23
Directory  ~/soil_carbon_runs/2024_calibration
Records    ~/soil_carbon_runs/2024_calibration/.stomp-archive/NSF-2231-20261118-2def23
Chunks     inside the directory, with the records
Grant      NSF-2231
Created    2026-11-18 18:04 by alex on cn0412
Finished   2026-11-18 18:04
Contents   36 files (2.4 MiB), 6 directories, 0 symlinks, 0 hard links
Stored as  3 chunk(s), 623.5 KiB (25% of the original), zstd level 19
```

Level 19 made the archive 25% of the original, against 33% at level 3.

## Archive small directories here

For a small directory, a Slurm task is more ceremony than the job needs.
`--here` runs the archive in the foreground instead. The marsh mapping grant
ended in June, and its 400 field photos only need to be kept:

```sh
stomp archive ~/marsh_mapping/field_photos --here
```

```text
Archive ~/marsh_mapping/field_photos (grant USDA-1180):
  packed into chunks of up to 50.0 GiB with zstd level 3, kept in
  ~/marsh_mapping/field_photos/.stomp-archive/USDA-1180-20261118-bbf6a9
Each chunk is read back and checked before the originals in it are removed.
Software environments, caches and files owned by others stay in place.
`--dry-run` shows the full plan; `--estimate` adds the expected size.
Archive it? [y/N] y
stomp: archiving here instead of in a Slurm task; Ctrl-C stops it safely and `--resume` continues.
Archived ~/marsh_mapping/field_photos: 400 files (800.0 KiB) compressed to 806.5 KiB in 1 chunk, in 0s.
Archive USDA-1180-20261118-bbf6a9. Restore it with `stomp restore USDA-1180-20261118-bbf6a9`.
```

JPEG photos are already compressed, so the archive is no smaller than the
photos. It is still worth doing: 400 files became 7, which helps if your
cluster limits the number of files you may keep.

Keep `--here` for small directories, and be considerate on shared login nodes.
Inside a Slurm job, such as your own batch script, stomp archives directly
without submitting another task, so `stomp archive DIR --yes` works as a step
in a job.

## What's next

- [Send archives to a NAS or Spaces](@/tutorials/nas-and-spaces.md) to move
  the chunks off the cluster.
- [Automate archiving with policies](@/tutorials/automate.md) to have stomp
  propose what to archive.
- [How archives work](@/concepts/archives.md) explains the safety rules behind
  each step.
- The [`stomp restore`](@/reference/commands.md#stomp-restore) reference lists
  every option.
