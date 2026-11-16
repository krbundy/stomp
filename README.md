# stomp

A storage management utility for HPC users. stomp runs as you, with no root or
admin help, and attributes your storage and Slurm jobs to the research grants
that pay for them.

The full design is in [`spec/design.md`](spec/design.md). This is milestone M1,
which provides:

| Command | What it does |
|---------|--------------|
| `stomp init` | Writes a starter config listing your top-level directories |
| `stomp doctor` | Checks the config and environment |
| `stomp usage` | Storage per grant, instant on CephFS; each run is saved to the history |
| `stomp du [DIR] [-d N]` | Directory sizes with the grant each one belongs to |
| `stomp report history` | Storage per grant over time, from saved measurements |
| `stomp jobs log GRANT` | Records that a Slurm job belongs to a grant |

`usage`, `du` and `report` accept `--format table|csv|json`.

## Building

```sh
cargo build --release              # target/release/stomp
```

For a static binary that runs on any Linux cluster regardless of its glibc
version (needs `musl-tools` for the bundled SQLite):

```sh
rustup target add x86_64-unknown-linux-musl
cargo build --release --target x86_64-unknown-linux-musl
```

Copy the binary to `~/bin` on the cluster.

## Getting started

```sh
stomp init          # writes ~/.config/stomp/config.toml
$EDITOR ~/.config/stomp/config.toml
stomp doctor
stomp usage
```

A grant claims one or more directories, and everything under them counts
toward it:

```toml
[[grant]]
id    = "GRANT-A"
title = "Example grant"
end   = 2027-06-30
dirs  = ["~/project_alpha", "~/project_alpha_runs"]
```

To attribute compute jobs, add one line to a job script:

```sh
#!/bin/bash
#SBATCH --time=04:00:00
stomp jobs log GRANT-A
srun ./model
```

`stomp jobs log` never makes a job fail. Problems are printed as warnings and
it exits 0, unless you pass `--strict`. Compute reports arrive in milestone M2.
