+++
title = "Browse your storage with stomp tui"
description = "An ncdu-style browser for sizes, grants, history and actions."
weight = 30

[extra]
time = "10 minutes"
needs = "a working config (see the first tutorial); a scan helps but isn't required"
+++

`stomp tui` is an interactive browser over your roots, in the spirit of
`ncdu`: one screen per directory, largest first, with each entry's grant.
You'll move around, look up a directory's history, assign a directory to a
grant without opening an editor, and estimate an archive, all from one screen.

## Start it

```sh
stomp tui
```

```text
 stomp ~
 1.7 TiB in 1,365 files · sorted by size
      SIZE                  FILES   AGE GRANT        NAME
 913.9 GiB ██████████         457   now USDA-1180    marsh_mapping/  [proposed #1]
 509.6 GiB ██████····          37   now NSF-2231     soil_carbon_runs/
 179.2 GiB ██········          46   now EPSCOR-T2    forest_plots/
  82.8 GiB █·········         100   now NSF-2231     soil_carbon/
  59.0 GiB █·········           2   now              scratch/
   7.3 GiB ··········         501   now              miniconda3/
   5.0 GiB ··········          82   now              containers/
   3.5 GiB ··········         120   now              .cache/  [cache]
 202.9 KiB ··········          11   now              .local/
   3.5 KiB ··········           7   now              papers/  [archived unassigned-20261118-b91c75]
     959 B ··········           1   now              .config/
      28 B ··········               now              .bashrc
↑↓ move  →/⏎ open  ← back  s sort  g grant  i info  e estimate  a archive  ? help  q quit
```

It opens at your root (or a list of roots, if you have several). Give it a
directory to start somewhere else: `stomp tui ~/soil_carbon_runs`.

**Each row** shows the size, a bar relative to the largest entry, the file
count, how long ago anything in it changed (AGE), its grant, and its name. In
a real terminal, names and grants are colored per grant, and environments,
caches and archive data are grey.

**The marks** after a name say what stomp knows about it:

| Mark | Meaning |
|------|---------|
| `[environment]`, `[cache]`, `[excluded]`, `[stomp data]` | Counted in usage, never scanned in detail or archived |
| `[archived ID]` | Holds a finished stomp archive; `[archiving ID]` while it's unfinished |
| `[archive data]` | The `.stomp-archive` directory itself |
| `[grants A, B]` | Not a grant directory, but contains some |
| `[proposed #N]` | A policy proposes archiving it; see `stomp candidates` |

**Sizes on CephFS are live**: they come from the filesystem's own totals, so
any directory opens instantly. On other filesystems stomp measures each
directory in the background as you browse, shows "measuring" in the header
until it's done, and remembers the results for the session. Press `R` to
measure again.

The bottom line lists the keys that fit your terminal's width; `?` shows all
of them.

## Move around

| Keys | Do |
|------|----|
| `↓` `↑` or `j` `k` | Move the selection |
| `PgDn` `PgUp` `Home` `End` | Move faster |
| `→`, `Enter` or `l` | Open the selected directory |
| `←`, `Backspace` or `h` | Go back up; the directory you came from stays selected |
| `s` | Sort by size, name, file count, or oldest first |
| `q` or `Esc` | Quit |

Open `soil_carbon_runs/`. The header now names the directory's grant and
when it ends:

```text
 stomp ~/soil_carbon_runs   grant NSF-2231 (ends 2027-08-31)
 509.6 GiB in 37 files · sorted by size
      SIZE                  FILES   AGE GRANT        NAME
 509.6 GiB ██████████          30   now NSF-2231     2025_scenarios/
 825.4 KiB ··········           7   now NSF-2231     2024_calibration/  [archived NSF-2231-20261118-…
```

`2024_calibration` was archived in place earlier, so it now holds a few
compressed chunks and records instead of 36 files.

## Look up a directory

Select `2024_calibration/` and press `i`:

```text
┌ 2024_calibration ──────────────────────────────────────────────────────────────────────┐
│Path      ~/soil_carbon_runs/2024_calibration                                           │
│Grant     NSF-2231 — Soil carbon dynamics under forest management (ends 2027-08-31)     │
│Size      825.4 KiB in 7 files                                                          │
│Changed   11m ago                                                                       │
│Note      archived NSF-2231-20261118-bde0da                                             │
│Archive   NSF-2231-20261118-bde0da, chunks inside the directory                         │
│                                                                                        │
│Scans                                                                                   │
│  2026-11-18     2.4 MiB            36 files                                            │
│                                                                                        │
│Press any key.                                                                          │
└────────────────────────────────────────────────────────────────────────────────────────┘
```

The info panel shows the grant, size, marks and archive, and the directory's
size in up to 8 past scans: the scan from before it was archived recorded
2.4 MiB in 36 files. For roots and grant directories it also lists the last
10 days of usage samples. Over months this is a quick way to see when a
directory grew.

## See compute per grant

Press `c` to open a panel at the bottom with the last 30 days of jobs per
grant. Press `c` again to close it.

```text
┌ Compute, last 30 days · accounting imported 10m ago ─────────────────────────────────────────────┐
│GRANT                JOBS    CPU-HOURS    GPU-HOURS                                               │
│NSF-2231               17        4,516          0.0                                               │
│EPSCOR-T2               4          117          0.0                                               │
│(unassigned)            6           68          0.0                                               │
│USDA-1180               1           31          7.7                                               │
└──────────────────────────────────────────────────────────────────────────────────────────────────┘
```

The grant of the directory you're in is highlighted. The panel uses the
accounting stomp has already imported; `stomp report compute` imports fresh
records and shows more.

## Assign a directory to a grant

Suppose Alex's `~/scratch` holds only soil-carbon intermediate files.
Instead of editing the config by hand, select `scratch/` and press `g`:

```text
  59.0 GiB █····┌ Grant for ~/scratch ─────────────────────────────────────────┐
   7.3 GiB ·····│NSF-2231  Soil carbon dynamics under forest management        │
   5.0 GiB ·····│USDA-1180  Coastal marsh mapping                              │
   3.5 GiB ·····│EPSCOR-T2  Forest plot remote sensing                         │
 202.9 KiB ·····└ ⏎ choose · Esc cancel ───────────────────────────────────────┘
```

Choose `NSF-2231` with Enter:

```text
  59.0 GiB █·········           2   now NSF-2231     scratch/
…
Added ~/scratch to grant NSF-2231 (the previous config is ~/.config/stomp/config.toml.bak).
```

stomp added `"~/scratch"` to that grant's `dirs` in `config.toml`, keeping
your comments and layout, and kept the old file as `config.toml.bak`:

```toml
dirs  = ["~/soil_carbon", "~/soil_carbon_runs", "~/scratch"]
```

The edit is checked like any config before it's saved. A change that would
break the rules (for example giving `~/soil_carbon/imagery` to `EPSCOR-T2`
while `~/soil_carbon` belongs to `NSF-2231`) is refused with the same message
`stomp doctor` would print, and nothing is written.

To undo it, press `g` on the same directory. The first choice is now
"Remove from grant NSF-2231":

```text
  59.0 GiB █····┌ Grant for ~/scratch ─────────────────────────────────────────┐
   7.3 GiB ·····│Remove from grant NSF-2231                                    │
   5.0 GiB ·····│USDA-1180  Coastal marsh mapping                              │
   3.5 GiB ·····│EPSCOR-T2  Forest plot remote sensing                         │
 202.9 KiB ·····└ ⏎ choose · Esc cancel ───────────────────────────────────────┘
```

{% note() %}
Moving a directory between grants moves its history too: usage samples and
scans are stored by path, and attributed using the current config. See
[Grants, roots and directories](@/concepts/grants-and-roots.md#changing-your-mind).
{% end %}

## Estimate, archive and restore

`e`, `a` and `r` work on the selected directory:

| Key | Runs |
|-----|------|
| `e` | `stomp archive DIR --estimate`: the plan and expected savings, changing nothing |
| `a` | `stomp archive DIR`: asks first, then submits an archive task |
| `r` | `stomp restore DIR`, on a directory marked `[archived …]` |

stomp's own checks run first. If the directory can't be archived (it's an
environment, it contains other grants' directories, it's already archived),
a message explains why, without leaving the browser.

Otherwise the browser steps aside and runs the same command the CLI would,
with the same output and the same `[y/N]` question. In `~/forest_plots`,
select `inventory/` and press `e`:

```text
stomp: planning lists every file under ~/forest_plots/inventory; on a big tree, run this inside a Slurm job.
Plan for archiving ~/forest_plots/inventory (grant EPSCOR-T2)

  files         30   735.5 KiB
  directories    0
  symlinks       0
  hard links     0
  chunks         1   up to 50.0 GiB each, zstd level 3

Estimate, from compressing 30 sampled files:
  compressed size   about 274.2 KiB (37% of the original), saving about 461.3 KiB
  files             30 → 7
  compression time  about 0s at 89 MB/s per thread with 8 thread(s); reading the files may take longer

Press Enter to return to stomp tui.
```

Press Enter and you're back where you were, with sizes measured again in
case something changed. Archiving with `a` works the same way: you see the
plan, answer the question, and the task is submitted; the directory shows
`[archiving …]` until it finishes.

## What's next

- [Find what's using your space](@/tutorials/find-space.md) with reports that
  need no browsing: stale data, caches, what changed.
- [Archive a finished project](@/tutorials/archive.md) explains what `a` and
  `r` do in detail.
- The [command reference](@/reference/commands.md#stomp-tui).
