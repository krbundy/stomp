+++
title = "Measure what compression saves"
description = "Test archiving on a sample of your own GeoTIFF and CSV files without touching them."
weight = 100

[extra]
time = "15 minutes, plus the job's run time"
needs = "stomp on the cluster, and the stomp repository (for scripts/stomp-test.sbatch)"
+++

Before archiving years of data, it's worth knowing how much it will actually
save. Text formats like CSV usually shrink a lot. GeoTIFFs vary: an
uncompressed raster shrinks well, one already compressed inside with LZW or
Deflate barely changes. This tutorial runs a Slurm job that copies a random
sample of your files, archives and restores **only the copies**, and reports
the savings and speed for each file type and compression level.

## What the test does

`scripts/stomp-test.sbatch`, in the stomp repository, is an ordinary batch
script. For each file type (GeoTIFF and CSV by default) it:

1. finds the matching files under the directories you name, and copies a
   random sample (up to 5 GB and 20,000 files per type) into a test folder;
2. for each compression level you ask for: estimates the size, archives the
   copies, verifies every file in the archive, restores it, and compares every
   restored file's contents, size, mode and modification time with the copy;
3. writes a summary, and a TSV with one row per type and level.

Your own files are only read. The test uses its own stomp config and data
directory, so nothing appears in your usage reports or archive list, and the
test folder is deleted at the end.

## Run it

Copy the script to the cluster (or clone the repository there), then submit
it with the directories to sample from:

```sh
sbatch scripts/stomp-test.sbatch ~/marsh_mapping ~/soil_carbon/data
```

Name a few project directories rather than all of `~`: finding files means
walking every directory you name.

If your cluster needs an account or partition, edit the two commented
`#SBATCH` lines at the top of the script first:

```sh
##SBATCH --partition=general
##SBATCH --account=your-account
```

Remove one `#` from each line you need and fill in the value.

## Choose the settings

Settings are environment variables in front of `sbatch`:

```sh
LEVELS="3 9 19" SAMPLE_GB=10 sbatch scripts/stomp-test.sbatch ~/marsh_mapping
```

| Variable | Default | Meaning |
|----------|---------|---------|
| `TYPES` | `"geotiff csv"` | File types to test: `geotiff` (`*.tif`, `*.tiff`) and/or `csv` |
| `SAMPLE_GB` | `5` | Sample size per type, in GB |
| `MAX_FILES` | `20000` | Most files per type |
| `LEVELS` | `"3"` | zstd levels to compare; 3 is stomp's default |
| `CHUNK_SIZE` | `2GiB` | Chunk size, small so a sample spans several chunks |
| `WORK` | `~/stomp-test-<job>` | The test folder. It needs about twice `SAMPLE_GB` free, and should be on the filesystem you'll archive |
| `KEEP` | `0` | `1` keeps the test folder afterwards |
| `CANDIDATES` | `200000` | How many matching files to pick the sample from |
| `STOMP` | `stomp` on PATH | The stomp binary to test |

The job asks for 8 CPUs, 8 GB and two hours; adjust the `#SBATCH` lines for
bigger samples or higher levels.

## Read the results

The output goes to `stomp-test-<job>.out` in the directory you submitted
from. It starts with the settings and a `stomp doctor` check, then logs each
step, and ends with a summary. This run sampled Alex's classified tiles and
CSV files, comparing levels 3 and 19:

```text
==== csv, zstd level 3

10:56:43  Estimating
10:56:43  Estimated: 37% of the original size
10:56:43  Archiving
Archived ~/stomp-test-local-2612085/data/csv: 51 files (4.2 MiB) compressed to 1.5 MiB in 1 chunk, in 0s.
Archive TEST-CSV-20261118-6b161d. Restore it with `stomp restore TEST-CSV-20261118-6b161d`.
10:56:44  Verifying every file in the archive
ok    chunk-0001.tar.zst
ok    manifest.jsonl.zst

All checks passed, including every file's checksum.
10:56:44  Restoring
Restored 51 files (4.2 MiB), 0 links and 1 directory into ~/stomp-test-local-2612085/data/csv.
Everything is back, so the archive was removed.
10:56:44  Comparing the restored files with the copies
10:56:44  Result: ok
…
==== Summary

geotiff  level 3        8 files    24.0 MiB → 10.0 MiB    stored  41.7%, saves 14.0 MiB    (estimated 42%)
         archive 1s (36.0 MB/s), verify 0s, restore 0s, 1 chunk(s)   ok

geotiff  level 19       8 files    24.0 MiB → 7.0 MiB     stored  29.2%, saves 17.0 MiB    (estimated 30%)
         archive 11s (2.4 MB/s), verify 0s, restore 0s, 1 chunk(s)   ok

csv      level 3       51 files     4.2 MiB → 1.5 MiB     stored  35.8%, saves 2.7 MiB     (estimated 37%)
         archive 0s (8.7 MB/s), verify 0s, restore 0s, 1 chunk(s)   ok

csv      level 19      51 files     4.2 MiB → 1.1 MiB     stored  27.5%, saves 3.0 MiB     (estimated 30%)
         archive 2s (2.1 MB/s), verify 0s, restore 0s, 1 chunk(s)   ok
```

For each type and level:

- **stored** is the archive's size as a share of the original, and **saves**
  what that frees. Here level 3 stored the CSV files in 36% of their size.
- **estimated** is what `stomp archive --estimate` predicted from a small
  sample. If it's close to *stored*, you can trust the estimate for the rest
  of your data without running this test again.
- **archive … MB/s** is how fast the whole job went: reading, compressing,
  verifying and removing. Multiply out to see how long your full data would
  take.
- **ok** means every restored file matched its copy exactly. Anything else
  means a test failed; the log above the summary says what differed, and the
  job exits non-zero so Slurm marks it failed.

For GeoTIFFs, the log also shows how the sampled files are compressed
internally (none, LZW, Deflate, JPEG, ZSTD, …), and what share of the bytes
each kind holds. Files that are already compressed inside won't shrink much
more, whatever the level.

## Choose a level

Higher levels save a little more and take much longer. In this sample, level
19 saved another 3 MiB on 24 MiB of rasters but archived 15 times slower. On
terabytes, that difference is days of compute. Level 3, stomp's default, is
usually the right choice; consider higher levels only for data you'll keep
archived for years and that compresses unusually well at them.

Set the level for all archives with `archive.zstd_level` in the config, or
for one archive with `stomp archive DIR --level N`.

## The spreadsheet

`stomp-test-<job>.tsv` has the same results, one row per type and level, with
raw numbers:

```text
type	level	files	original_bytes	stored_bytes	stored_pct	estimated_pct	chunks	archive_s	archive_MB_per_s	verify_s	restore_s	files_left_in_place	result
```

Open it in a spreadsheet to compare runs on different directories or
clusters.

## What's next

- [Archive a finished project](@/tutorials/archive.md) for real.
- [How archives work](@/concepts/archives.md) explains what the verify and
  compare steps guarantee.
- See how much of each file type a grant holds: run `stomp scan`, then
  `stomp report grant ID` and look at "File types".
