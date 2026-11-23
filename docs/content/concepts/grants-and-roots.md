+++
title = "Grants, roots and directories"
description = "How stomp decides which grant a file counts toward."
weight = 10
+++

stomp answers one question for every byte you store and every hour you
compute: which grant does it belong to? It answers it with three ideas.

## Roots

A **root** is a top-level directory stomp manages, usually your home
directory. You can have several, for example your home and a project space:

```toml
[general]
roots = ["~", "/project/rivera/alex"]
```

Everything under a root is counted somewhere: either toward a grant, or as
**unassigned**. Roots can't be nested inside each other, and stomp never
crosses from a root into another filesystem mounted inside it.

## Grant directories

A **grant** claims one or more directories, and **everything under them**
counts toward it: files, subdirectories, software environments, caches.

```toml
[[grant]]
id   = "NSF-2231"
end  = 2027-08-31
dirs = ["~/soil_carbon", "~/soil_carbon_runs"]
```

This matches how research data is usually organized, one directory tree per
project, and it makes attribution exact. On CephFS the filesystem keeps
running totals for every directory, so a grant's usage is just the sum of its
directories' totals, available instantly.

### One grant per directory

A directory belongs to at most one grant, and one grant's directory can't be
inside another grant's. stomp refuses a config that breaks this rule:

```text
~/soil_carbon/marsh_subset (grant USDA-1180) is inside ~/soil_carbon (grant NSF-2231); grant directories must not overlap
```

Overlaps would make the totals ambiguous: a file would either count twice or
need a rule for which grant wins. If two grants really share a project, give
each its own subdirectory, or choose the grant that pays for most of it.

Listing the same directory twice in one grant, or a directory inside another
of the same grant's, is harmless: stomp warns and counts it once.

### Directories must be inside a root

stomp resolves every path to its real location, following symlinks. If
`~/project` is a symlink to `/project/rivera/alex`, then `/project/rivera/alex`
has to be inside a root, so add it to `roots`.

## Unassigned

Whatever is under a root but outside every grant directory is
**unassigned**: your dotfiles, software in `~/miniconda3`, scratch work,
papers. Unassigned is often large, and seeing it is useful in itself.
`stomp du` shows which top-level directories make it up.

When you have several roots, `stomp usage` shows unassigned space per root.

## Excluded directories

Some directories are counted but treated specially:

| Kind | Examples |
|------|----------|
| environment | Apptainer/Singularity sandboxes, conda environments, Python venvs |
| cache | `~/.cache`, `~/.apptainer/cache`, `~/.singularity/cache`, `~/.conda/pkgs`, and `pkgs` inside `~/miniconda3`, `~/anaconda3`, `~/miniforge3` and `~/mambaforge` |
| excluded | anything matching `scan.exclude` |
| stomp data | stomp's own data and staging directories |

They count toward whichever grant (or unassigned) they're under, at full size,
but detailed scans don't walk inside them, and they're never archived. A
venv or sandbox holds thousands of files that can be rebuilt in minutes;
archiving them file by file isn't worth it. `stomp report reclaimable` lists
them with suggestions for slimming them down.

## Changing your mind

Grant attribution is computed from the **current** config every time. Usage
history, scans and job records are stored by path, not by grant. So if you
move `~/soil_carbon_runs` from one grant to another, its whole history moves
with it, and so do jobs attributed by working directory. Jobs you tagged
explicitly with `stomp jobs log` or `stomp jobs retag` keep their grant.

You can change the mapping by editing the config, or by pressing `g` in
[`stomp tui`](@/tutorials/browse.md), which edits the file for you.

## Grant IDs

A grant's `id` is what you type in commands and see in reports. Use the
funder's number or any short name: letters, digits, `-`, `_` and `.`, up to 64
characters. Two IDs are reserved: `unassigned`, and `stomp`, which compute
reports use for stomp's own overhead, such as scans.

A grant doesn't need directories. One with only an `id` (and maybe dates) can
still collect compute jobs logged to it.
