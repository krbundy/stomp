+++
title = "Close out a grant that has ended"
description = "Report a finished grant's final numbers, archive its data off the cluster, and keep the records."
weight = 90

[extra]
time = "25 minutes"
needs = "a config with the grant's end date, and a NAS destination that works (see Send archives to a NAS or Spaces)"
+++

`USDA-1180`, M. Chen's coastal marsh mapping grant, ended on 2026-06-30, and
its data is still in `~/marsh_mapping`. In this tutorial Alex checks what the
grant still uses, saves its final numbers for the final report, moves the data
to the lab NAS, and makes sure it can come back. At the end the directory
holds only a note and the archive's records, and stomp's reports show where
the data went.

## Check what the grant still uses

`stomp report lifecycle` lists your grants by end date:

```sh
stomp report lifecycle
```

```text
Grant end dates, with storage as of 2026-11-18 18:07 EST.

GRANT       ENDS         STATUS                   STORAGE ON HPC   ARCHIVED ELSEWHERE   JOBS AFTER END   CPU-H AFTER END
USDA-1180   2026-06-30   ended 141 days ago            913.9 GiB                    —                3                92
NSF-2231    2027-08-31   active (286 days left)        592.5 GiB                    —                —                 —
EPSCOR-T2   2028-05-31   active (560 days left)        179.2 GiB                    —                —                 —
```

Four and a half months after its end, `USDA-1180` still holds 914 GiB on the
cluster, as of the newest measurement. Nothing of it is archived elsewhere
yet, and three jobs attributed to it started after its end date, using 92
CPU-hours between them. A grant whose end is 90 days away or less shows
`ends in N days` here, which is a good time to plan the same steps.

## Look at jobs that ran after the end date

Which jobs were they?

```sh
stomp jobs list --grant USDA-1180 --since 2026-07-01
```

```text
JOB       NAME         STATE       ENDED              CPU-H   GPU-H   GRANT       BY
4799140   marsh_unet   COMPLETED   2026-09-23 12:15      27     6.7   USDA-1180   workdir
4799033   marsh_unet   COMPLETED   2026-10-03 12:27      35     8.7   USDA-1180   workdir
4799270   marsh_unet   COMPLETED   2026-11-13 10:35      31     7.7   USDA-1180   workdir
```

`BY` says `workdir`: these jobs ran from `~/marsh_mapping`, so stomp counted
them toward the grant that owns the directory. An end date doesn't stop that.
Alex reran the marsh model for the paper's revision this autumn, and J. Rivera
agreed to cover it from `NSF-2231`. `stomp jobs retag` moves the jobs there:

```sh
stomp jobs retag NSF-2231 4799140 4799033 4799270
```

```text
Tagged 3 job(s) as NSF-2231.
```

```sh
stomp report lifecycle
```

```text
Grant end dates, with storage as of 2026-11-18 18:07 EST.

GRANT       ENDS         STATUS                   STORAGE ON HPC   ARCHIVED ELSEWHERE   JOBS AFTER END   CPU-H AFTER END
USDA-1180   2026-06-30   ended 141 days ago            913.9 GiB                    —                0               0.0
NSF-2231    2027-08-31   active (286 days left)        592.5 GiB                    —                —                 —
EPSCOR-T2   2028-05-31   active (560 days left)        179.2 GiB                    —                —                 —
```

If you keep running jobs from an ended grant's directory for other work, add
`stomp jobs log` with the paying grant to those job scripts, as
[Attribute compute jobs to grants](@/tutorials/compute.md) shows, so they're
right from the start.

## Import the grant's whole job history

The first time stomp imported Slurm accounting, it went back 90 days, so it
doesn't know about the grant's earlier jobs yet. For the final numbers, tell it
to go back to the grant's start by adding `history_start` to a `[compute]`
section of your config:

```toml
[compute]
history_start = 2022-07-01   # the start of the oldest grant
```

The next import fills in everything from that date that stomp doesn't have.
Run one now:

```sh
stomp jobs sync
```

```text
Imported 64 job record(s) from sacct, covering 2022-07-01 00:00 EDT to now. 138 job(s) in the catalog.
```

stomp can only import what Slurm still has. How far back that goes depends on
how long your site keeps accounting records; stomp keeps its own copy from now
on.

## Get the final numbers

`stomp report grant` gathers the grant's storage and compute in one report:

```sh
stomp report grant USDA-1180
```

```text
Grant USDA-1180: Coastal marsh mapping
======================================

PI: M. Chen · 2022-07-01 to 2026-06-30
Storage history and compute cover 2022-07-01 to 2026-11-18.

Storage
-------
Measured 2026-11-18 18:07 EST.
DIRECTORY              SIZE   FILES
~/marsh_mapping   913.9 GiB     457

Storage by month
----------------
MONTH          SIZE
2026-06   892.9 GiB
2026-07   913.9 GiB
2026-08   913.9 GiB
2026-09   913.9 GiB
2026-10   913.9 GiB
2026-11   913.9 GiB
Storage over the period: 5.48 TB-months, about 24.67 at the configured rate (estimated: measurements are more than two days apart in places).

Data age
--------
By last modification, from the scan of 2026-11-18 18:07 EST. Software environments and caches aren't included.
LAST MODIFIED         SIZE   FILES   SHARE
under 30 days          0 B       0      0%
30–90 days             0 B       0      0%
90 days–1 year   695.1 GiB      48     76%
1–3 years        218.8 GiB     409     24%
over 3 years           0 B       0      0%

File sizes
----------
FILE SIZE        FILES   TOTAL SIZE
under 4 KiB        401    800.0 KiB
4 KiB–1 MiB          8      1.7 MiB
1–100 MiB            8     24.0 MiB
100 MiB–10 GiB       0          0 B
over 10 GiB         40    913.9 GiB

File types
----------
Top 10, from the full scan of 2026-11-18 18:07 EST.
EXTENSION   FILES        SIZE
tif            48   913.9 GiB
csv             8     1.7 MiB
jpg           400   800.0 KiB
md              1        16 B

Largest files
-------------
    SIZE   MODIFIED     PATH
24.7 GiB   2025-10-10   ~/marsh_mapping/orthomosaics/flight_2024_19.tif
24.6 GiB   2025-10-16   ~/marsh_mapping/orthomosaics/flight_2024_18.tif
24.5 GiB   2025-10-22   ~/marsh_mapping/orthomosaics/flight_2024_17.tif
…

Compute by month
----------------
MONTH     JOBS   CPU-HOURS   GPU-HOURS
2025-09      4         726         0.0
2025-10      6         921         0.0
2025-11      3         452         0.0
2025-12      3         322         0.0
2026-01      6         444         9.9
2026-02     11         858          21
2026-03      8         638          33
2026-04     12         403          30
2026-05      7         298         0.0
2026-06      1          50          13
--------------------------------------
TOTAL       61       5,114         106
About 77.64 at the configured rates.
```

By default the report covers the whole grant, from its start date to today,
as the line under the title says. `--from` and `--to` choose another period.

- **Storage** is 914 GiB in 457 files. Storage by month only starts in June,
  when Alex began recording measurements, and the line below it sums the
  storage over the period: 5.48 TB-months, about 24.67 at the department's
  rate. It's marked estimated because the early measurements were a month
  apart; [Report usage to your PI or department](@/tutorials/reports.md)
  explains TB-months.
- **Data age** shows that no file in the directory was modified in the last
  90 days.
- **File sizes** and **Largest files** show where the space is: the 40 drone
  orthomosaics in `~/marsh_mapping/orthomosaics`, each over 10 GiB. The rest
  is small, the classified tiles (`.tif` and `.csv`, 25.7 MiB) and 400 field
  photos.
- **Compute by month** has every job of the grant's, from the first ones in
  September 2025 to the end, with the total: 61 jobs, 5,114 CPU-hours and
  106 GPU-hours, about 77.64 at the configured rates. The three autumn jobs
  no longer appear, since they now belong to `NSF-2231`.

It's also worth knowing what keeping the data after the end has cost. Ask the
history for the grant since July:

```sh
stomp report history --grant USDA-1180 --from 2026-07-01
```

```text
MEASURED               USDA-1180
2026-07-01 09:05 EDT   913.9 GiB
2026-08-03 08:50 EDT   913.9 GiB
2026-09-01 09:20 EDT   913.9 GiB
2026-10-01 09:00 EDT   913.9 GiB
2026-11-02 09:15 EST   913.9 GiB
2026-11-03 03:41 EST   913.9 GiB
…
2026-11-18 03:41 EST   913.9 GiB
2026-11-18 18:07 EST   913.9 GiB

TB-months over this period
--------------------------
GRANT       TB-MONTHS   ESTIMATED COST
USDA-1180       4.53*            20.37
Storage held over time, in decimal terabytes (10¹² bytes) for an average month, integrated between measurements. * Estimated: measurements are up to 33 days apart, so the storage in between is interpolated. `stomp schedule enable` records one a day.
```

At the department's rate of 4.50 per TB-month, the data has cost about 4.40 a
month since the grant ended, with no grant left to pay for it.

## Save the report for the final report

Save the grant report as Markdown now, before you change anything. Once the
data is gone, the Storage section shows what's left, and the month's entry in
Storage by month shows the final, much smaller measurement.

```sh
stomp report grant USDA-1180 --format md > ~/USDA-1180-final-report.md
head -13 ~/USDA-1180-final-report.md
```

```text
# Grant USDA-1180: Coastal marsh mapping

PI: M. Chen · 2022-07-01 to 2026-06-30

Storage history and compute cover 2022-07-01 to 2026-11-18.

## Storage

Measured 2026-11-18 18:07 EST.

| DIRECTORY | SIZE | FILES |
| --- | ---: | ---: |
| ~/marsh_mapping | 913.9 GiB | 457 |
```

The file holds every section of the report, the compute totals included. Send
it to M. Chen for the grant's final report.

## Remove what's already kept elsewhere

As the grant required, the orthomosaics were deposited in a public data
repository when the project's paper came out. The repository is now the
copy that counts, so there's no reason to pay for a second one on the NAS.
Alex deletes them:

```sh
rm -r ~/marsh_mapping/orthomosaics
```

{% note(kind="warning", title="Deleting is permanent") %}
Before you delete anything, check that the deposit is complete: every file is
there, with the right size or checksum, and you know its identifier. If the
data management plan or your PI says to keep a copy of everything, archive the
whole directory instead.
{% end %}

What's left, the classified tiles and the field photos, isn't in the
repository, so it goes to the NAS.

## Estimate the archive

`--estimate` shows the plan and predicts the compressed size, without
changing anything:

```sh
stomp archive ~/marsh_mapping --to nas --estimate
```

```text
stomp: planning lists every file under ~/marsh_mapping; on a big tree, run this inside a Slurm job.
Plan for archiving ~/marsh_mapping (grant USDA-1180)

  files         417   26.5 MiB
  directories     2
  symlinks        0
  hard links      0
  chunks          1   up to 50.0 GiB each, zstd level 3

Estimate, from compressing 14 sampled files:
  compressed size   about 11.1 MiB (42% of the original), saving about 15.4 MiB
  files             417 → 6
  compression time  about 0s at 214 MB/s per thread with 8 thread(s); reading the files may take longer
```

417 files, 26.5 MiB, fit in one chunk, and should compress to less than half.
Afterwards 6 files stay in the directory: the note and the archive's records,
which you'll see below. It's a small archive, but the point is to get it off
the cluster, verified and with a record of where it went.
[Archive a finished project](@/tutorials/archive.md) explains the plan and
the estimate in detail.

## Archive to the NAS

Now archive for real. `--to nas` sends the chunks to the `nas` destination
from your config:

```sh
stomp archive ~/marsh_mapping --to nas
```

```text
Archive ~/marsh_mapping (grant USDA-1180):
  packed into chunks of up to 50.0 GiB with zstd level 3, sent to
  nas:/archive/alex/USDA-1180/USDA-1180-20261118-cc46a2
  (the archive's records stay in ~/marsh_mapping/.stomp-archive/USDA-1180-20261118-cc46a2)
Each chunk is verified at nas before the originals in it are removed.
Software environments, caches and files owned by others stay in place.
`--dry-run` shows the full plan; `--estimate` adds the expected size.
Archive it? [y/N] y
Submitted archive task archive-20261118-180747-176f as Slurm job 4810234. Follow it with `stomp tasks`.
Archive ID: USDA-1180-20261118-cc46a2
```

The question says where the chunks go, on the NAS under the grant's ID, and
where the archive's records stay, in the directory itself.

Archiving runs as a Slurm task, using the `[slurm]` settings in your config.
For each chunk, the task writes it, reads it back and checks every file,
uploads it, checks it again on the NAS, and only then removes the originals
in it. Follow it with `stomp tasks`:

```sh
stomp tasks
```

```text
TASK                           STATUS   SLURM JOB   UPDATED            PROGRESS
scan-20261118-180727-8eb5      done     4810233     2026-11-18 18:07   1,387 files, 1.7 TiB
archive-20261118-180747-176f   done     4810234     2026-11-18 18:07   417 files, 26.5 MiB
```

A task still in progress shows `running`, with how far it has got. The task
runs on a compute node, so the NAS has to be reachable from there. If it isn't
on your cluster, archive locally in a task instead and move the archive to the
NAS from a login node, as
[Send archives to a NAS or Spaces](@/tutorials/nas-and-spaces.md#when-compute-nodes-can-t-reach-the-nas)
describes.

## Check the archive

`stomp archives` lists your archives:

```sh
stomp archives
```

```text
ARCHIVE                     STATE      ORIGINAL   COMPRESSED   FILES   CREATED            WHERE   DIRECTORY
USDA-1180-20261118-cc46a2   complete   26.5 MiB     11.5 MiB     417   2026-11-18 18:07   nas     ~/marsh_mapping
```

`complete` means the archive finished: every chunk was verified on the NAS
before the originals in it were removed. WHERE says where the chunks are:
`nas` here, `directory` for an archive kept in its own directory. For the
details of one archive, use `stomp archives show`:

```sh
stomp archives show USDA-1180-20261118-cc46a2
```

```text
Archive    USDA-1180-20261118-cc46a2
Directory  ~/marsh_mapping
Records    ~/marsh_mapping/.stomp-archive/USDA-1180-20261118-cc46a2
Chunks     nas:/archive/alex/USDA-1180/USDA-1180-20261118-cc46a2
Grant      USDA-1180
Created    2026-11-18 18:07 by alex on cn0412
Finished   2026-11-18 18:07
Contents   417 files (26.5 MiB), 2 directories, 0 symlinks, 0 hard links
Stored as  1 chunk(s), 11.5 MiB (43% of the original), zstd level 3
```

`cn0412` is the compute node the task ran on. The task already checked
everything on the way, but it costs little to check again now, and again
before you rely on the archive years from now:

```sh
stomp archives verify USDA-1180-20261118-cc46a2
```

```text
ok    chunk-0001.tar.zst
ok    manifest.jsonl.zst

All checks passed.
```

For a chunk on the NAS, `verify` asks the NAS for its SHA-256 checksum over
SSH and compares it with the one recorded when the archive was made. (That's
the default; if the NAS doesn't allow commands, stomp reads the chunk back
instead.) The manifest is checked against its recorded checksum too. `--deep`
goes further: it downloads every chunk, decompresses it and checks every
file's checksum, which takes as long as a restore.

## See what stays in the directory

```sh
ls -A ~/marsh_mapping
```

```text
.stomp-archive
STOMP_ARCHIVED.txt
```

`STOMP_ARCHIVED.txt` is for anyone who opens the directory, a collaborator or
you in three years:

```sh
cat ~/marsh_mapping/STOMP_ARCHIVED.txt
```

```text
This directory was archived by stomp on 2026-11-18 18:07 EST by alex.

Archive:   USDA-1180-20261118-cc46a2
Location:  nas:/archive/alex/USDA-1180/USDA-1180-20261118-cc46a2
           (records in .stomp-archive/USDA-1180-20261118-cc46a2, inside this directory)
Contents:  417 files (26.5 MiB), compressed to 11.5 MiB in 1 chunk(s)

The originals were packed into compressed tar files and removed.

To restore with stomp:

    stomp restore USDA-1180-20261118-cc46a2

To restore without stomp, see .stomp-archive/USDA-1180-20261118-cc46a2/README.txt.
```

The hidden `.stomp-archive` directory holds the archive's records:

```sh
ls ~/marsh_mapping/.stomp-archive/USDA-1180-20261118-cc46a2
```

```text
journal.jsonl
manifest.jsonl.zst
README.txt
settings.json
SHA256SUMS
```

The manifest lists every archived file with its checksum and which chunk holds
it, so stomp can list an archive and plan a restore without contacting the
NAS. `README.txt` explains how to restore with only `sha256sum`, `zstd` and
`tar`. They're small: with the note, 23 KiB here. Keep them: without them,
`stomp restore` can't find the archive, and you'd have to restore by hand from
the copies of the manifest and README on the NAS.

stomp's reports now show the move. `stomp usage` lists what's held elsewhere
below its usual table:

```sh
stomp usage
```

```text
…
Archived elsewhere

DESTINATION   GRANT       ARCHIVES   ORIGINAL     STORED
nas           USDA-1180          1   26.5 MiB   11.5 MiB
```

```sh
stomp report lifecycle
```

```text
Grant end dates, with storage as of 2026-11-18 18:07 EST.

GRANT       ENDS         STATUS                   STORAGE ON HPC   ARCHIVED ELSEWHERE   JOBS AFTER END   CPU-H AFTER END
USDA-1180   2026-06-30   ended 141 days ago             23.1 KiB             11.5 MiB                0               0.0
NSF-2231    2027-08-31   active (286 days left)        592.5 GiB                    —                —                 —
EPSCOR-T2   2028-05-31   active (560 days left)        179.2 GiB                    —                —                 —
```

The 23.1 KiB left on the cluster is the note and the records. The grant
report gains an Archives section, and says that the sections from the last
scan predate the archive:

```sh
stomp report grant USDA-1180
```

```text
…
Data age
--------
By last modification, from the scan of 2026-11-18 18:07 EST. Software environments and caches aren't included.
1 archive(s) finished since this scan, so the sections from it still include their data; run `stomp scan` for current numbers.
…
Archives
--------
ARCHIVE                     WHERE   STATE      ORIGINAL     STORED   DIRECTORY
USDA-1180-20261118-cc46a2   nas     complete   26.5 MiB   11.5 MiB   ~/marsh_mapping

…
```

## Restore one file later

A year on, a reviewer asks about tile 3. You don't need the whole archive
back. `--only` picks paths inside the archived directory, and `--to` puts them
somewhere else, leaving `~/marsh_mapping` as it is. Patterns are relative to
the archived directory: `*` doesn't cross a `/`, and a pattern that names a
directory, such as `field_photos`, takes everything in it. Quote patterns so
the shell doesn't expand them. `--only` can be given more than once.

stomp checks the patterns against the manifest before it does anything else,
so a mistake shows up at once, with nothing submitted or created:

```sh
stomp restore USDA-1180-20261118-cc46a2 --only 'classified_tiles/tile_3.*' --to ~/marsh_restore
```

```text
stomp: error: nothing in the archive matches --only "classified_tiles/tile_3.*"; patterns are relative to the archived directory, e.g. `results` or `results/*.csv`
```

The tiles are numbered with two digits. `--dry-run` shows what a restore would
do without doing it:

```sh
stomp restore USDA-1180-20261118-cc46a2 --only 'classified_tiles/tile_03.*' --to ~/marsh_restore --dry-run
```

```text
Restoring archive USDA-1180-20261118-cc46a2 into ~/marsh_restore:
  files                2   3.2 MiB
  directories          1
  links                0
  chunks               1   (1 to download from nas)
Nothing was changed (--dry-run).
```

Two files match, in one chunk, which has to come from the NAS. If any files
were already there and differed from the archive, the plan would list them as
conflicts. Now restore them:

```sh
stomp restore USDA-1180-20261118-cc46a2 --only 'classified_tiles/tile_03.*' --to ~/marsh_restore
```

```text
Submitted restore task restore-20261118-180756-2c50 as Slurm job 4810235. Follow it with `stomp tasks`.
```

The restore runs as a Slurm task too. It downloads only the chunks that hold
matching files, checks each chunk's checksum and each file's after
extracting it. When it's done, `stomp tasks show` has the result:

```sh
stomp tasks show restore-20261118-180756-2c50
```

```text
Task      restore-20261118-180756-2c50
Kind      restore
Status    done
Created   2026-11-18 18:07
Updated   2026-11-18 18:07
Jobs      4810235
Message   Restored 2 files (3.2 MiB), 0 links and 1 directory into ~/marsh_restore.
The archive was kept.
Script    ~/.local/share/stomp/tasks/restore-20261118-180756-2c50/task.sh
Log       ~/.local/share/stomp/tasks/restore-20261118-180756-2c50/slurm-4810235.out
```

```sh
ls ~/marsh_restore/classified_tiles
```

```text
tile_03.csv
tile_03.tif
```

The files keep their path inside the archive, below the `--to` directory, and
their original modification times. A restore that runs into conflicts it
leaves alone ends as `partial` instead of `done`. The archive is untouched: a
partial restore, or one with `--to`, never removes it. For a small restore
like this one you can add `--here` to run it in the foreground instead of as a
task.

To bring everything back where it was, run `stomp restore` with just the ID,
as the note says. The NAS copy is kept unless you add `--delete-remote`, and
the archive's records move to `~/.local/share/stomp/manifests/`, so
`stomp archives show`, `stomp archives verify` and `stomp restore` keep
working with that copy. When you no longer need it,
`stomp archives delete USDA-1180-20261118-cc46a2` deletes it from the NAS,
after asking.

## Keep the grant in your config

It's tempting to delete the `[[grant]]` block for `USDA-1180` now. Don't: it
costs nothing, and removing it changes what stomp can tell you about the
grant.

- `stomp report grant USDA-1180` stops working, with
  `no grant "USDA-1180" in ~/.config/stomp/config.toml`.
- The grant drops out of `stomp report lifecycle`.
- Jobs attributed by working directory become `(unassigned)`, including all
  61 of the grant's jobs. Jobs tagged with `stomp jobs log` show as
  `USDA-1180 (unknown)`.
- `~/marsh_mapping`, with its note and records, counts as unassigned
  storage.

Some things survive either way: the storage history keeps a `USDA-1180` column
for measurements taken while the grant was in the config, and the archive
keeps its grant, in `stomp archives`, in `stomp usage` under "Archived
elsewhere", and on the NAS, where it's stored under `USDA-1180/`. But the
report you'd most want later is the one you'd lose.

So leave the block as it is, end date included. A comment helps whoever reads
the config next:

```toml
[[grant]]
id    = "USDA-1180"
title = "Coastal marsh mapping"
pi    = "M. Chen"
start = 2022-07-01
end   = 2026-06-30
dirs  = ["~/marsh_mapping"]   # archived to nas 2026-11-18: USDA-1180-20261118-cc46a2
```

`stomp report lifecycle` will keep listing the grant as ended, with a few
kilobytes on the cluster and its archive on the NAS, which is exactly the
record you want.

## What's next

- [Archive a finished project](@/tutorials/archive.md) covers archiving in
  more depth, including local archives and resuming.
- [Send archives to a NAS or Spaces](@/tutorials/nas-and-spaces.md) sets up
  destinations and moves existing archives between them.
- [Automate archiving with policies](@/tutorials/automate.md) can propose
  directories like `~/marsh_mapping` automatically, a set time after their
  grant ends.
- [How archives work](@/concepts/archives.md) explains the safety checks
  behind every archive.
