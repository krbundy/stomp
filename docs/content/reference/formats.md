+++
title = "Sizes, durations and output"
description = "How to write sizes, durations and dates, and the table, CSV, JSON and Markdown outputs."
weight = 50
+++

## Sizes

Sizes in the config and on the command line (`--chunk-size`, `min_bytes`,
`max_avg_file_size`, `chunk_size`, `fold_bytes`) are a number and a unit.
The number may have a decimal point (`"1.5TiB"`), units are case-insensitive,
and a space between number and unit is allowed.

| Unit | Means | Example |
|------|-------|---------|
| none, `B` | bytes | `"4096"` |
| `K`, `KiB` | 1,024 bytes | `"512KiB"` |
| `M`, `MiB` | 1,024² bytes | `"1MiB"` |
| `G`, `GiB` | 1,024³ bytes | `"50GiB"`, `"50G"` |
| `T`, `TiB` | 1,024⁴ bytes | `"2TiB"` |
| `P`, `PiB` | 1,024⁵ bytes | |
| `KB`, `MB`, `GB`, `TB`, `PB` | powers of 1,000 | `"500GB"` |

stomp always prints sizes in binary units (KiB, MiB, GiB, TiB), the way `du
-h` and most HPC tools do.

`--mem` is passed straight to Slurm, so it uses Slurm's syntax (`16G`).

## Durations

Durations (`--since`, `--older-than`, `full_every`, `rctime_margin`,
`grant_ended_for`, `unchanged_for`, `wait`) are a whole number and a unit:

| Unit | Means |
|------|-------|
| `s` | seconds |
| `m` | minutes |
| `h` | hours |
| `d` | days |
| `w` | weeks |
| `y` | years of 365 days |

For example `"90d"`, `"6w"`, `"1y"`. There's no month unit, because months
differ in length; use `"30d"` or `"180d"`.

`--time` is passed straight to Slurm, so it uses Slurm's syntax
(`12:00:00`, `2-00:00:00`).

## Dates

- **In the config**, dates are bare TOML dates without quotes:
  `end = 2027-06-30`.
- **On the command line**, `--from` and `--to` take `YYYY-MM-DD`.
- **`--since` and `--older-than`** take either a duration (`30d`) or a date
  (`2026-10-01`).
- **Times are shown in your local time zone** (`TZ`, or the system's), as
  `2026-11-18 10:26 EST`. Ages are shown as `just now`, `5m ago`, `3h ago`,
  `41d ago` or `2y ago`.

## Output formats {#output}

Commands that print tables take `--format table|csv|json|md`. The global
`--json` flag is shorthand for `--format json`.

### table

For reading in a terminal: aligned columns, human sizes, a heading line that
says when the data was measured or scanned.

```text
GRANT          DIRS        SIZE   FILES   LAST CHANGE
USDA-1180         1   913.9 GiB     457   3m ago
NSF-2231          2   592.5 GiB     166   3m ago
```

### md

The same tables as Markdown, ready to paste into a progress report, an issue
or a wiki page:

```text
| GRANT | DIRS | SIZE | FILES | LAST CHANGE |
| --- | ---: | ---: | ---: | --- |
| USDA-1180 | 1 | 913.9 GiB | 457 | 3m ago |
| NSF-2231 | 2 | 592.5 GiB | 166 | 3m ago |
```

### csv

For spreadsheets and scripts: one header row, then one row per item, with raw
numbers (bytes, not `913.9 GiB`; hours and costs rounded to two decimals) and
times in RFC 3339 (UTC):

```text
measured_at,grant,roots,bytes,files,last_change
2026-11-18T15:35:10.415686815Z,NSF-2231,/home/alex,636168154539,166,2026-11-18T15:31:44Z
2026-11-18T15:35:10.415686815Z,USDA-1180,/home/alex,981306145902,457,2026-11-18T15:31:49Z
```

### json

Structured data with raw numbers, for `jq` and scripts. Timestamps are
RFC 3339 strings or Unix seconds:

```text
{
  "taken_at": "2026-11-18T15:35:10.475464348Z",
  "roots": [
    {
      "path": "/home/alex",
      "backend": "posix",
      "total": { "bytes": 1890228338406, "files": 1394, ... },
…
```

```sh
# Bytes per grant, from a script
stomp usage --json --no-record | jq -r '.grants[] | "\(.id) \(.total.bytes)"'
```

Use `--no-record` when a script calls `stomp usage` often, so it doesn't add a
sample to your history every time.
