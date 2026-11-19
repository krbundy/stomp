+++
title = "Report usage to your PI or department"
description = "One grant's storage and compute, history with TB-months, and output for spreadsheets and documents."
weight = 50

[extra]
time = "20 minutes"
needs = "a config with your grants (see the first tutorial) and a few weeks of usage measurements"
+++

Sooner or later someone asks how much storage and compute a grant uses: your
PI for a progress report, or the department for its chargeback. In this
tutorial Alex prepares the numbers for `NSF-2231`, checks which grants are
ending, and gets the same reports out as a Markdown document, a CSV file for a
spreadsheet, and JSON for a script.

## Keep a history of measurements

Reports about the past can only use measurements stomp has saved. Every time
you run `stomp usage` (or `stomp report usage`, which is the same command), it
measures your storage per grant, prints the table described in
[Read your first usage report](@/tutorials/first-setup.md#read-your-first-usage-report),
and saves the numbers to its history:

```sh
stomp usage
```

Pass `--no-record` when you only want to look and not save. Alex ran
`stomp usage` at the start of each month from June, then turned on the
[daily run](@/tutorials/automate.md) in November, which records a measurement
every night. So there's a history to report on, and you'll see below why the
daily measurements are better.

## Report on one grant

`stomp report grant` puts everything stomp knows about one grant on one page:

```sh
stomp report grant NSF-2231
```

```text
Grant NSF-2231: Soil carbon dynamics under forest management
============================================================

PI: J. Rivera · 2024-09-01 to 2027-08-31
Storage history and compute cover 2024-09-01 to 2026-11-18.

Storage
-------
Measured 2026-11-18 18:05 EST.
DIRECTORY                 SIZE   FILES
~/soil_carbon         82.8 GiB     100
~/soil_carbon_runs   509.6 GiB      66
--------------------------------------
TOTAL                592.5 GiB     166

Storage by month
----------------
MONTH          SIZE
2026-06    37.0 GiB
2026-07    47.5 GiB
2026-08    60.8 GiB
2026-09   206.0 GiB
2026-10   394.4 GiB
2026-11   592.5 GiB
Storage over the period: 1.43 TB-months, about 6.45 at the configured rate (estimated: measurements are more than two days apart in places).

Data age
--------
By last modification, from the scan of 2026-11-18 18:05 EST. Software environments and caches aren't included.
LAST MODIFIED         SIZE   FILES   SHARE
under 30 days     42.6 GiB       6      7%
30–90 days       343.9 GiB      24     58%
90 days–1 year   206.0 GiB      35     35%
1–3 years          2.9 MiB      40      0%
over 3 years           0 B       0      0%

File sizes
----------
FILE SIZE        FILES   TOTAL SIZE
under 4 KiB         14     19.0 KiB
4 KiB–1 MiB         37      4.1 MiB
1–100 MiB            0          0 B
100 MiB–10 GiB      24     82.8 GiB
over 10 GiB         30    509.6 GiB

Environments and caches
-----------------------
Counted in the grant's storage but not scanned or archived.
KIND              SIZE   PATH
environment   70.3 KiB   ~/soil_carbon/.venv

File types
----------
Top 10, from the full scan of 2026-11-18 18:05 EST.
EXTENSION   FILES        SIZE
nc             30   509.6 GiB
tif            24    82.8 GiB
csv            37     4.1 MiB
log             6    18.8 KiB
yaml            6       216 B
py              1        37 B
ipynb           1         3 B

Largest files
-------------
    SIZE   MODIFIED     PATH
20.0 GiB   2026-08-13   ~/soil_carbon_runs/2025_scenarios/scenario_29/state.nc
19.8 GiB   2026-08-16   ~/soil_carbon_runs/2025_scenarios/scenario_28/state.nc
19.6 GiB   2026-08-19   ~/soil_carbon_runs/2025_scenarios/scenario_27/state.nc
…

Compute by month
----------------
MONTH     JOBS   CPU-HOURS   GPU-HOURS
2026-09      9         963         0.0
2026-10     15       3,524         0.0
2026-11     12       3,119         0.0
--------------------------------------
TOTAL       36       7,607         0.0
About 76.07 at the configured rates.
```

The title, PI and dates come from the grant's entry in your config. The next
line gives the report's period: by default from the grant's start date (or a
year ago, if the grant has none) to today. `--from` and `--to` choose another
period, for example one reporting year. Then, in order:

- **Storage** is each of the grant's directories at the newest measurement.
- **Storage by month** is the last measurement of each month in the period.
  `NSF-2231` has grown from 37 GiB to almost 600 GiB since June, as the 2025
  model scenarios came in. The line below it adds up the storage over the
  period in TB-months, with its cost at your configured rate; the next section
  explains TB-months and why these are marked estimated.
- **Data age**, **File sizes**, **Environments and caches**, **File types** and
  **Largest files** come from the newest [scan](@/concepts/scans.md). Data age
  sorts the grant's data by when each file was last modified, which tells a
  reader how much of it is in active use. Without a scan, the report says so
  and suggests `stomp scan` instead.
- **Archives** lists the grant's archives and where they're kept. `NSF-2231`
  has none yet, so the section is left out.
- **Compute by month** is the CPU-hours and GPU-hours of the jobs attributed to
  the grant in the period, as in
  [Attribute compute jobs to grants](@/tutorials/compute.md), with a total and
  its cost at your configured rates.

The report can only cover what stomp has recorded. Alex's storage history
starts with the first measurement in June, and the jobs start in September
because stomp imports 90 days of accounting by default; setting
[`history_start`](@/tutorials/compute.md#set-rates-and-compute-options) fills
in older jobs. Like the compute reports, the grant report imports new job
accounting first if the last import is more than 15 minutes old.

## See storage over time, in TB-months

`stomp report history` shows every saved measurement, one row each, with a
column per grant:

```sh
stomp report history
```

```text
MEASURED               EPSCOR-T2    NSF-2231   USDA-1180   (unassigned)       TOTAL
2026-06-01 09:10 EDT         0 B    37.0 GiB   892.9 GiB       20.7 GiB   950.7 GiB
2026-07-01 09:05 EDT         0 B    47.5 GiB   913.9 GiB       20.7 GiB   982.1 GiB
2026-08-03 08:50 EDT         0 B    60.8 GiB   913.9 GiB       23.3 GiB   998.1 GiB
2026-09-01 09:20 EDT    13.4 GiB   206.0 GiB   913.9 GiB       26.9 GiB     1.1 TiB
2026-10-01 09:00 EDT    99.0 GiB   394.4 GiB   913.9 GiB       26.9 GiB     1.4 TiB
2026-11-02 09:15 EST   179.2 GiB   549.9 GiB   913.9 GiB       26.9 GiB     1.6 TiB
2026-11-03 03:41 EST   179.2 GiB   564.3 GiB   913.9 GiB       26.9 GiB     1.6 TiB
2026-11-04 03:41 EST   179.2 GiB   564.3 GiB   913.9 GiB       26.9 GiB     1.6 TiB
…
2026-11-17 03:41 EST   179.2 GiB   592.5 GiB   913.9 GiB       74.9 GiB     1.7 TiB
2026-11-18 03:41 EST   179.2 GiB   592.5 GiB   913.9 GiB       74.9 GiB     1.7 TiB
2026-11-18 18:05 EST   179.2 GiB   592.5 GiB   913.9 GiB       74.9 GiB     1.7 TiB

TB-months over this period
--------------------------
GRANT          TB-MONTHS   ESTIMATED COST
EPSCOR-T2          0.33*             1.47
NSF-2231           1.43*             6.45
USDA-1180          5.48*            24.67
(unassigned)       0.15*             0.68
TOTAL              7.39*            33.27
Storage held over time, in decimal terabytes (10¹² bytes) for an average month, integrated between measurements. * Estimated: measurements are up to 33 days apart, so the storage in between is interpolated. `stomp schedule enable` records one a day.
```

The monthly measurements come first, then one every night at 03:41 from the
daily run, and the last row is the `stomp usage` run from a moment ago.

A snapshot says how much a grant holds today; storage charges depend on how
much it held and for how long. That's what a TB-month measures: one terabyte
kept for one month. stomp works it out from the measurements. For each pair of
measurements in a row, it takes the average of the two sizes and multiplies it
by the time between them, then adds up the pieces. As the note under the table
says, the terabytes are decimal, 10<sup>12</sup> bytes, a little smaller than
the TiB in the sizes above; a month is 30.44 days, an average month.

`USDA-1180`, for example, held about 914 GiB, or 0.98 TB, for the 5.6 months
from June 1 to today: 5.48 TB-months. At Alex's department's rate of 4.50 per
TB-month, that's the 24.67 in the ESTIMATED COST column, which appears when
your config has a `rate_per_tb_month` (see
[Rates and cost columns](#rates-and-cost-columns) below).

Because stomp only knows the measurements, it assumes storage changed evenly
between them. A `*` marks a number as estimated: somewhere in the period,
measurements are more than two days apart, here up to 33 days, over the
monthly part of the history. A grant needs at least two measurements in the
period to get a TB-months row at all.

The rows are labelled by your current config: if you move a directory to
another grant, its whole history moves with it.

To report on one grant over part of the history, add `--grant`, `--from` and
`--to` (dates; both days are included). Since the daily run started, there's a
measurement every day, so these numbers aren't estimated:

```sh
stomp report history --grant NSF-2231 --from 2026-11-03
```

```text
MEASURED                NSF-2231
2026-11-03 03:41 EST   564.3 GiB
2026-11-04 03:41 EST   564.3 GiB
2026-11-05 03:41 EST   564.3 GiB
2026-11-06 03:41 EST   578.5 GiB
…
2026-11-17 03:41 EST   592.5 GiB
2026-11-18 03:41 EST   592.5 GiB
2026-11-18 18:05 EST   592.5 GiB

TB-months over this period
--------------------------
GRANT      TB-MONTHS   ESTIMATED COST
NSF-2231        0.32             1.45
Storage held over time, in decimal terabytes (10¹² bytes) for an average month, integrated between measurements.
```

The TB-months cover the time from the first to the last measurement inside the
range, not the whole range. With a measurement every day, that's as close to
the calendar period as makes no difference.

When you only need the totals, `--tb-months` leaves out the measurements:

```sh
stomp report history --tb-months
```

```text
TB-months over this period
--------------------------
GRANT          TB-MONTHS   ESTIMATED COST
EPSCOR-T2          0.33*             1.47
NSF-2231           1.43*             6.45
USDA-1180          5.48*            24.67
(unassigned)       0.15*             0.68
TOTAL              7.39*            33.27
Storage held over time, in decimal terabytes (10¹² bytes) for an average month, integrated between measurements. * Estimated: measurements are up to 33 days apart, so the storage in between is interpolated. `stomp schedule enable` records one a day.
```

## Check which grants are ending

`stomp report lifecycle` lists every grant by its end date:

```sh
stomp report lifecycle
```

```text
Grant end dates, with storage as of 2026-11-18 18:05 EST.

GRANT       ENDS         STATUS                   STORAGE ON HPC   ARCHIVED ELSEWHERE   JOBS AFTER END   CPU-H AFTER END
USDA-1180   2026-06-30   ended 141 days ago            913.9 GiB                    —                3                92
NSF-2231    2027-08-31   active (286 days left)        592.5 GiB                    —                —                 —
EPSCOR-T2   2028-05-31   active (560 days left)        179.2 GiB                    —                —                 —
```

- **STATUS** says how long ago a grant ended or how long it has left. A grant
  with 90 days or fewer left shows `ends in N days`.
- **STORAGE ON HPC** is the grant's storage at the newest measurement.
- **ARCHIVED ELSEWHERE** is the compressed size of the grant's archives kept at
  remote destinations, such as a NAS.
- **JOBS AFTER END** and **CPU-H AFTER END** count jobs attributed to the grant
  that started after its end date. They're only shown for grants that have
  ended.

`USDA-1180` ended in June but still holds 914 GiB, and three jobs have run
from its directory since. [Close out a grant that has
ended](@/tutorials/grant-closeout.md) walks through what to do about both.

## Put the numbers in a document

Every report takes `--format`: `table` (the default), `md`, `csv` or `json`.
Markdown suits anything a person will read. Save the grant report to a file:

```sh
stomp report grant NSF-2231 --format md > NSF-2231-2026-11.md
head -28 NSF-2231-2026-11.md
```

```text
# Grant NSF-2231: Soil carbon dynamics under forest management

PI: J. Rivera · 2024-09-01 to 2027-08-31

Storage history and compute cover 2024-09-01 to 2026-11-18.

## Storage

Measured 2026-11-18 18:05 EST.

| DIRECTORY | SIZE | FILES |
| --- | ---: | ---: |
| ~/soil_carbon | 82.8 GiB | 100 |
| ~/soil_carbon_runs | 509.6 GiB | 66 |
| TOTAL | 592.5 GiB | 166 |

## Storage by month

| MONTH | SIZE |
| --- | ---: |
| 2026-06 | 37.0 GiB |
| 2026-07 | 47.5 GiB |
| 2026-08 | 60.8 GiB |
| 2026-09 | 206.0 GiB |
| 2026-10 | 394.4 GiB |
| 2026-11 | 592.5 GiB |

Storage over the period: 1.43 TB-months, about 6.45 at the configured rate (estimated: measurements are more than two days apart in places).
```

Each section becomes a heading and each table a Markdown table. Paste it into
a GitHub or GitLab issue, a wiki page or a lab notebook, and the tables render
as tables. If your PI wants a Word document, `pandoc` converts the file:
`pandoc NSF-2231-2026-11.md -o NSF-2231-2026-11.docx`.

The other reports work the same way: `stomp report history --format md` gives
the history table, then the TB-months table under a
`## TB-months over this period` heading.

## Put the numbers in a spreadsheet

`--format csv` prints one flat table with a header row, ready for a
spreadsheet:

```sh
stomp report history --format csv > storage-history.csv
head -6 storage-history.csv
```

```text
measured_at,grant,bytes,files
2026-06-01T13:10:00Z,(unassigned),22239878435,285
2026-06-01T13:10:00Z,EPSCOR-T2,0,0
2026-06-01T13:10:00Z,NSF-2231,39781831872,119
2026-06-01T13:10:00Z,TOTAL,1020779277889,859
2026-06-01T13:10:00Z,USDA-1180,958757567582,455
```

CSV is meant for machines, so it differs from the table:

- There's one row per grant per measurement, instead of a column per grant,
  with `(unassigned)` and `TOTAL` as if they were grants. A spreadsheet's
  pivot table turns it back into the wide layout.
- Sizes are exact byte counts. Divide by 10<sup>12</sup> for TB, or by
  2<sup>40</sup> for TiB.
- Times are in UTC, in the RFC 3339 format (`2026-06-01T13:10:00Z` is 09:10 in
  New York), which spreadsheets and scripts parse without guessing.
- The TB-months aren't in it; the CSV holds the measurements only.

For the TB-months in a spreadsheet, add `--tb-months`:

```sh
stomp report history --tb-months --format csv
```

```text
grant,tb_months,estimated_cost,estimated,longest_gap_days
EPSCOR-T2,0.33,1.47,true,33.0
NSF-2231,1.43,6.45,true,33.0
USDA-1180,5.48,24.67,true,33.0
(unassigned),0.15,0.68,true,33.0
TOTAL,7.39,33.27,true,33.0
```

`estimated` is `true` where the table shows a `*`, and `longest_gap_days` is the
longest time between two measurements, which is what made it so.

The compute report has more columns in CSV than on screen:

```sh
stomp report compute --from 2026-10-01 --to 2026-10-31 --format csv
```

```text
grant,jobs,not_completed,running,cpu_hours,gpu_hours,billing_hours,estimated_cost,by_log,by_task,by_workdir
NSF-2231,15,0,0,3524.35,0.0,3524.35,35.24,0,0,15
EPSCOR-T2,4,1,0,121.98,6.3,121.98,2.8,0,0,4
USDA-1180,1,0,0,34.76,8.69,34.76,2.52,0,0,1
(unassigned),6,0,0,71.04,0.0,71.04,0.71,0,0,0
```

Hours and costs have two decimals instead of the table's rounding, `running`
counts jobs still running, and the `by_*` columns split ATTRIBUTED BY into
numbers. There's no TOTAL row; let the
spreadsheet add up the columns.

The grant report has several tables with different columns, so it can't be one
CSV table:

```sh
stomp report grant NSF-2231 --format csv
```

```text
stomp: error: the grant report has several sections; use --format table, md or json
```

## Use the numbers in a script

`--format json` prints the same data as JSON. `--json` is a shorter way to say
it, and works wherever `--format` does:

```sh
stomp report lifecycle --json
```

```text
[
  {
    "grant": "USDA-1180",
    "end": "2026-06-30",
    "status": "ended 141 days ago",
    "days": -141,
    "storage_bytes": 981306145902,
    "archived_remote_bytes": 0,
    "jobs_after_end": 3,
    "cpu_hours_after_end": 92.18
  },
  {
    "grant": "NSF-2231",
…
]
```

As in CSV, sizes are in bytes and hours have two decimals. Pipe the output to
`jq` to pick out what you need, for example the grants that have ended but
still hold data:

```sh
stomp report lifecycle --json | jq -r '.[] | select(.days < 0 and .storage_bytes > 0) | .grant'
```

```text
USDA-1180
```

The history's JSON holds both parts, the measurements and the TB-months:

```sh
stomp report history --json --grant USDA-1180 --from 2026-11-17
```

```text
{
  "measurements": [
    {
      "measured_at": "2026-11-17T08:41:00Z",
      "grant": "USDA-1180",
      "bytes": 981306145902,
      "files": 457
    },
…
  ],
  "tb_months": [
    {
      "grant": "USDA-1180",
      "tb_months": 0.05,
      "estimated_cost": 0.23,
      "estimated": false,
      "longest_gap_days": 1.0
    }
  ]
}
```

[Sizes, durations and output](@/reference/formats.md) describes the output
formats in more detail.

## Rates and cost columns

The cost columns are estimates from rates you put in the `[general]` section
of your config. Alex's department charges back storage and compute, so Alex's
config has:

```toml
[general]
roots = ["~"]
rate_per_tb_month = 4.50     # what the department charges back, per TB-month
rate_per_cpu_hour = 0.01
rate_per_gpu_hour = 0.25
```

- `rate_per_tb_month` adds ESTIMATED COST to the TB-months in
  `stomp report history`: TB-months times the rate. In `stomp report grant`,
  it adds the cost to the "Storage over the period" line.
- `rate_per_cpu_hour` and `rate_per_gpu_hour` add EST. COST to
  `stomp report compute`: CPU-hours times one rate plus GPU-hours times the
  other. In `stomp report grant`, they give the "About … at the configured
  rates" line under Compute by month.

Each rate is optional; without it, its costs disappear. The usage and
lifecycle reports never show costs. stomp doesn't know what your
site charges, so check the rates with whoever sends the bills before you put
the costs in a report.

## What's next

- [Close out a grant that has ended](@/tutorials/grant-closeout.md) uses these
  reports to wrap up `USDA-1180`.
- [Automate archiving with policies](@/tutorials/automate.md) sets up the
  daily run, which records a measurement every day and keeps the TB-months
  accurate.
- [How stomp measures storage](@/concepts/measuring.md) explains where the
  numbers come from.
- The [`stomp report`](@/reference/commands.md#stomp-report) reference lists
  every report and option.
