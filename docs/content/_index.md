+++
title = "stomp"
description = "Track storage and compute per research grant on HPC clusters, and archive what you're done with."

[extra]
headline = "Know what each grant is paying for."
tagline = "stomp is a command-line tool for HPC users. It attributes your storage and Slurm jobs to the research grants that fund them, finds what's using space, and archives finished work safely, to your own directory, a NAS or object storage. It runs as you, with no admin help."
features = [
    { title = "Storage per grant", text = "Say which directories belong to which grant. `stomp usage` adds them up, instantly on CephFS, and keeps a history.", link = "@/tutorials/first-setup.md" },
    { title = "Compute per grant", text = "CPU and GPU hours from Slurm accounting, attributed by where each job ran or by one line in the job script.", link = "@/tutorials/compute.md" },
    { title = "Find what's using space", text = "Directory sizes, stale data, caches and environments, what changed since last month, and an ncdu-style browser.", link = "@/tutorials/find-space.md" },
    { title = "Archive safely", text = "Pack finished work into verified, compressed chunks. Nothing is deleted until every file's checksum has been compared.", link = "@/tutorials/archive.md" },
    { title = "Off the cluster", text = "Send archives to a NAS over SFTP or to S3-compatible storage such as DigitalOcean Spaces, verified at the other end.", link = "@/tutorials/nas-and-spaces.md" },
    { title = "On autopilot", text = "Policies propose what to archive; a small daily Slurm job keeps usage, accounting and proposals current.", link = "@/tutorials/automate.md" },
]
+++

## Why stomp

Many HPC centers don't bill storage or compute by project. If you work on
several grants, you still have to report what each one used, decide what to
keep, and clean up when a grant ends. stomp does that bookkeeping for you:

- **One config file** says which directories belong to which grant. Everything
  under a grant's directories counts toward it.
- **Your own permissions only.** stomp runs as you, keeps its state in your
  home directory, and never needs root, a daemon or a site-wide database.
- **Heavy work runs as Slurm jobs.** Scans and archives are submitted as
  ordinary jobs under your account, never as long processes on a login node.
- **Archives never depend on stomp.** Each one carries a `README.txt` that
  explains how to restore it with `sha256sum`, `zstd` and `tar`.

## Where to start

- New to stomp? [Install it](@/getting-started/install.md), then follow the
  [quick start](@/getting-started/quick-start.md). It takes about ten minutes.
- Want to learn by doing? The [tutorials](@/tutorials/_index.md) walk through
  real tasks, from setting up your grants to closing out a grant that has ended.
- Looking for a flag or a config key? See the
  [command reference](@/reference/commands.md) and the
  [configuration reference](@/reference/configuration.md).
