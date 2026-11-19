+++
title = "Configuration"
description = "Every key in config.toml, with defaults."
weight = 20
+++

stomp reads one [TOML](https://toml.io) file, `~/.config/stomp/config.toml`
(or `$XDG_CONFIG_HOME/stomp/config.toml`). Pass `--config PATH` to any command
to use another. `stomp init` writes a starter file, and `stomp doctor` checks
it.

A few rules apply everywhere:

- **Unknown keys are errors**, so a typo like `dir = [...]` instead of
  `dirs = [...]` is caught instead of silently ignored.
- **Paths** may start with `~/` and may contain environment variables such as
  `$USER` or `${SCRATCH}`. `~user` isn't supported. Paths are resolved to
  their real location, following symlinks.
- **Sizes** are written like `"50GiB"` or `"1MiB"`, **durations** like
  `"90d"` or `"1y"`, and **dates** as bare TOML dates like `2027-06-30` (no
  quotes). See [Sizes, durations and output](@/reference/formats.md).

If the file has errors, commands stop, list every problem along with any
warnings, and exit with code 2 (`stomp jobs log` still records the job, with a
warning). Warnings (such as a grant directory that doesn't
exist) are printed but don't stop anything.

## A complete example

```toml
[general]
roots = ["~"]
# staging_dir = "/scratch/$USER/stomp-staging"
rate_per_tb_month = 4.50
rate_per_cpu_hour = 0.01
rate_per_gpu_hour = 0.25

[slurm]
account   = "lab-rivera"
partition = "standard"
time      = "08:00:00"
cpus      = 8
mem       = "16G"

[scan]
exclude = ["**/node_modules", "**/__pycache__"]

[archive]
default_destination = "nas"

[[grant]]
id    = "NSF-2231"
title = "Soil carbon dynamics under forest management"
pi    = "J. Rivera"
start = 2024-09-01
end   = 2027-08-31
dirs  = ["~/soil_carbon", "~/soil_carbon_runs"]

[[grant]]
id    = "USDA-1180"
title = "Coastal marsh mapping"
end   = 2026-06-30
dirs  = ["~/marsh_mapping"]

[[destination]]
name = "nas"
type = "sftp"
host = "nas"
path = "/archive/$USER"

[[destination]]
name     = "spaces"
type     = "s3"
endpoint = "https://nyc3.digitaloceanspaces.com"
bucket   = "rivera-lab-archives"
prefix   = "stomp/$USER"
profile  = "spaces"

[[policy]]
name        = "ended-grants"
match       = { grant_ended_for = "90d" }
destination = "spaces"
```

## [general] {#general}

| Key | Default | Meaning |
|-----|---------|---------|
| `roots` | `["~"]` | The top-level directories stomp manages. Each must exist, and roots must not be nested inside each other. Everything under a root that isn't in a grant directory is reported as unassigned. |
| `staging_dir` | none | Where chunks for a remote destination are built before upload, and where downloaded chunks wait during a restore or move. Without it, that happens inside the archived directory. Point it at scratch space if your home quota is tight. |
| `rate_per_tb_month` | none | Cost of storing 1 TB for a month. Adds cost estimates to storage reports. |
| `rate_per_cpu_hour` | none | Cost of one CPU-hour. Adds an estimated cost to compute reports. |
| `rate_per_gpu_hour` | none | Cost of one GPU-hour. |

Rates are plain numbers in whatever currency you use; they must not be
negative.

## [slurm] {#slurm}

Settings for the Slurm tasks stomp submits (scans, archives, restores, moves).
Each can be overridden for one command with `--time`, `--cpus`, `--mem`,
`--partition` or `--account`.

| Key | Default | Meaning |
|-----|---------|---------|
| `account` | site default | Slurm account to charge. |
| `partition` | site default | Slurm partition. |
| `time` | site default | Time limit, e.g. `"12:00:00"`. A task that reaches it saves its progress and continues in a new job. |
| `cpus` | `scan.max_threads` (8) | CPUs per task. |
| `mem` | site default | Memory per task, e.g. `"16G"`. Archiving holds the list of files in memory, about 1.5 GB per 10 million files. |
| `auto_resubmit` | `true` | Resubmit a task that reached its time limit, up to 5 times. With `false`, continue it yourself with `--resume`. |

The daily run is always small (1 CPU, 2 GB, 30 minutes) and uses only
`account` and `partition` from here.

## [scan] {#scan}

| Key | Default | Meaning |
|-----|---------|---------|
| `max_threads` | `8` | How many files and directories a scan examines in parallel. This protects the shared metadata server. Inside a Slurm job it's also capped by the CPUs allocated. |
| `full_every` | `"30d"` | On CephFS, how often a scan walks everything instead of skipping unchanged directories. |
| `rctime_margin` | `"1h"` | On CephFS, the safety margin when deciding a directory hasn't changed since the last scan. |
| `exclude` | `[]` | Glob patterns for directories to count in usage but never scan in detail or archive. They match whole absolute paths: `"**/node_modules"` matches any directory named `node_modules`. `*` doesn't cross `/`, `**` does, and a leading `~` is expanded. |
| `fold_bytes` | `"1MiB"` | A directory smaller than this **and** with fewer than `fold_files` files is recorded as part of its parent, which keeps the catalog small. |
| `fold_files` | `100` | See `fold_bytes`. |

Software environments (Apptainer sandboxes, conda environments, Python venvs)
and the caches `~/.cache`, `~/.apptainer/cache`, `~/.singularity/cache`,
`~/.conda/pkgs` and the `pkgs` directory of `~/miniconda3`, `~/anaconda3`,
`~/miniforge3` and `~/mambaforge` are recognized without any configuration. See
[Scans and the catalog](@/concepts/scans.md).

## [archive] {#archive}

| Key | Default | Meaning |
|-----|---------|---------|
| `chunk_size` | `"50GiB"` | The largest chunk. A file is never split, so a file bigger than this gets a chunk to itself. At least `"1MiB"`. |
| `zstd_level` | `3` | Compression level, 1–22. Higher levels compress a little more and take much longer. |
| `only_owned` | `true` | Leave files owned by other users in place instead of archiving and removing them. |
| `default_destination` | `"local"` | Where `stomp archive` sends chunks when you don't give `--to`: `"local"` (inside the directory) or a destination's name. |

## [compute] {#compute}

| Key | Default | Meaning |
|-----|---------|---------|
| `infer_from_workdir` | `true` | Attribute a job to a grant when its working directory is inside that grant's directories. |
| `history_start` | 90 days ago | How far back imports from `sacct` go, as a date like `2025-01-01`. Setting it earlier later on fills in the older jobs at the next import. |

See [Compute attribution](@/concepts/compute.md).

## [[grant]] {#grant}

One block per grant. Every file under a grant's directories counts toward it.

| Key | Required | Meaning |
|-----|----------|---------|
| `id` | yes | A short name used in commands and reports, such as `NSF-2231`. Letters, digits, `-`, `_` and `.`, up to 64 characters, not starting with `-` or `.`. `stomp` and `unassigned` are reserved. |
| `title` | no | The grant's title, shown in reports. |
| `pi` | no | The principal investigator. |
| `start` | no | Start date, e.g. `2024-09-01`. |
| `end` | no | End date. Used by `stomp report lifecycle` and the `grant_ended_for` policy condition. |
| `dirs` | no | The grant's directories. A grant with no directories still collects compute jobs. |

The rules for `dirs`:

- **Each directory must be inside one of the roots.** A symlink is resolved
  first, so a link in your home pointing to project space only works if
  project space is a root too.
- **A directory belongs to one grant.** Two grants claiming the same directory,
  or one grant's directory inside another grant's, is an error.
- **Within one grant**, a directory listed twice, or inside another of the
  same grant's directories, is a warning; it's counted once.
- **A directory that doesn't exist** is a warning, since it may have been
  renamed or moved. A path that exists but isn't a directory is an error.

See [Grants, roots and directories](@/concepts/grants-and-roots.md).

## [[destination]] {#destination}

A named place where archive chunks can be stored. Every destination has:

| Key | Meaning |
|-----|---------|
| `name` | The name used with `--to`, in `default_destination` and in policies. Unique, no spaces. `local` is reserved for archives kept inside the directory. |
| `type` | `"sftp"` or `"s3"`. |

See [Destinations](@/concepts/destinations.md) for how each type stores and
verifies chunks.

### SFTP {#sftp}

A server you can reach with `sftp`, such as a NAS. stomp runs your system's
`ssh` and `sftp`, so your `~/.ssh/config` (host aliases, `ProxyJump`, keys,
the agent) applies. Login must work without a password prompt.

| Key | Default | Meaning |
|-----|---------|---------|
| `host` | required | Anything `sftp` accepts: `nas`, `alex@nas.example.edu`, or an alias from `~/.ssh/config`. |
| `path` | required | Base directory on the server, e.g. `"/archive/$USER"`. A leading `~` means your home directory **on the server**, so `"~/stomp"` works on a NAS where you don't know the absolute path. Archives go in `<path>/<grant>/<archive-id>/`. |
| `verify` | automatic | How each upload is checked. Automatic: run `sha256sum` on the server over `ssh`, and if the server doesn't allow commands, download the file again and hash it. `"remote-sha256"` requires the first; `"readback"` always downloads; `"size"` compares sizes only (weak; avoid it). |

SFTP uploads use one stream at a time; `parallel_uploads` applies to S3 only,
and setting it here is a warning.

### S3 {#s3}

S3 or an S3-compatible service such as DigitalOcean Spaces, MinIO or campus
object storage.

| Key | Default | Meaning |
|-----|---------|---------|
| `bucket` | required | The bucket name. |
| `endpoint` | AWS | The service's URL, e.g. `"https://nyc3.digitaloceanspaces.com"`. Just the scheme and host. Default: `https://s3.<region>.amazonaws.com`. |
| `region` | `"us-east-1"` | The signing region. Spaces ignores it. |
| `prefix` | none | A folder inside the bucket, e.g. `"stomp/$USER"`. Archives go in `<prefix>/<grant>/<archive-id>/`. |
| `profile` | see below | Which section of `~/.aws/credentials` (or `~/.aws/config`) holds the keys. |
| `force_path_style` | `false` | Address the bucket as `endpoint/bucket` instead of `bucket.endpoint`. Some self-hosted services need it. |
| `storage_class` | `STANDARD` | Only `"STANDARD"` is accepted for now. Cold classes such as Glacier need thawing before a restore, which stomp doesn't do yet, so they're an error. |
| `parallel_uploads` | `4` | How many parts of a large chunk upload at once, 1–16. Keep it modest on services with rate limits. |
| `verify` | `"server-checksum"` | `"server-checksum"`: the server checks every request's SHA-256 and MD5, and stomp checks the stored object's size and checksum. `"readback"`: download and hash each object (costs egress). `"size"`: sizes only (weak). |

**Credentials.** With no `profile`, stomp uses `AWS_ACCESS_KEY_ID` and
`AWS_SECRET_ACCESS_KEY` (and `AWS_SESSION_TOKEN`) from the environment if
they're set. Otherwise it reads the profile named by `profile`, or
`$AWS_PROFILE`, or `default`, from `~/.aws/credentials` and then
`~/.aws/config`. `AWS_SHARED_CREDENTIALS_FILE` and `AWS_CONFIG_FILE` move
those files.

```ini
# ~/.aws/credentials (chmod 600)
[spaces]
aws_access_key_id = DO00EXAMPLEKEY
aws_secret_access_key = example-secret
```

## [[policy]] {#policy}

A rule that proposes directories to archive. See
[Policies and the daily run](@/concepts/policies.md).

| Key | Default | Meaning |
|-----|---------|---------|
| `name` | required | Shown in `stomp candidates`. Letters, digits, `-`, `_` and `.`; unique. |
| `match` | required | The conditions, as a table: `match = { grant_ended_for = "90d" }`. At least one. |
| `action` | `"propose"` | `"propose"` lists matches in `stomp candidates` for you to apply. `"apply"` lets the daily run archive them automatically after `wait`. |
| `destination` | `archive.default_destination` | Where archives from this policy go: `"local"` or a destination's name. |
| `wait` | `"7d"` | With `action = "apply"`: how long after a directory is first proposed before it's archived automatically. |

Conditions in `match`. All of them must hold:

| Condition | Matches a directory when |
|-----------|--------------------------|
| `grant` | it belongs to this grant, or one of a list: `grant = ["NSF-2231", "USDA-1180"]`. |
| `path` | its path matches this glob, e.g. `"~/scratch/*"`. `*` doesn't cross `/`, `**` does. Without `path`, only grant data is considered (see below). |
| `grant_ended_for` | its grant's `end` date is at least this long ago, e.g. `"90d"`. |
| `unchanged_for` | nothing in it was modified **or** changed (mtime and ctime) for at least this long, e.g. `"1y"`. |
| `min_bytes` | it holds at least this much, e.g. `"10GiB"`. |
| `min_files` | it holds at least this many files, e.g. `10000`. |
| `max_avg_file_size` | its average file size is at most this, e.g. `"1MiB"`: lots of small files. |

- **Outermost directories.** A policy proposes the outermost directories that
  meet every condition, since archiving a directory covers everything in it.
- **Grant data only, unless you give a `path`.** Without `path`, only
  directories inside grant directories are proposed, and nothing under a hidden
  directory. Software and reference data can look untouched for years while
  being read every day.
- **`action = "apply"` needs an age condition** (`grant_ended_for` or
  `unchanged_for`), so only data nobody is changing is archived automatically.
