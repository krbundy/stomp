use std::collections::HashSet;
use std::path::{Path, PathBuf};

use jiff::civil::date;

use super::*;
use crate::scan::ExcludeKind;
use crate::scan::hist::Agg;

const DAY: i64 = 86_400;
/// "Now" for these tests: 2026-11-17, and days before it.
const NOW: i64 = 1_795_392_000;

struct Fixture {
    _tmp: tempfile::TempDir,
    root: PathBuf,
}

impl Fixture {
    fn new() -> Fixture {
        let tmp = tempfile::tempdir().unwrap();
        let root = std::fs::canonicalize(tmp.path()).unwrap();
        for d in ["ended", "active/runs", "active/.hidden", "loose", "both/a"] {
            std::fs::create_dir_all(root.join(d)).unwrap();
        }
        Fixture { _tmp: tmp, root }
    }

    fn config(&self, policies: &str) -> Config {
        let text = format!(
            r#"
            general.roots = ["ROOT"]
            [[grant]]
            id = "OLD"
            end = 2025-06-30
            dirs = ["ROOT/ended"]
            [[grant]]
            id = "NEW"
            end = 2027-06-30
            dirs = ["ROOT/active"]
            {policies}
            "#
        )
        .replace("ROOT", self.root.to_str().unwrap());
        Config::parse(&text, Path::new("test.toml")).unwrap()
    }

    fn p(&self, rel: &str) -> PathBuf {
        if rel.is_empty() {
            self.root.clone()
        } else {
            self.root.join(rel)
        }
    }

    /// A recorded directory: its subtree size, file count and newest change, in days ago.
    fn row(&self, rel: &str, bytes: u64, files: u64, days_ago: i64) -> DirRow {
        let agg = Agg {
            bytes,
            files,
            newest_mtime: Some(NOW - days_ago * DAY),
            newest_change: Some(NOW - days_ago * DAY),
            ..Agg::default()
        };
        DirRow::new(&self.p(rel), None, None, bytes, files, agg)
    }
}

fn run(
    cfg: &Config,
    rows: &[DirRow],
    archived: &[PathBuf],
    protected: &[PathBuf],
) -> Vec<Candidate> {
    let archived: HashSet<PathBuf> = archived.iter().cloned().collect();
    let inputs = Inputs {
        cfg,
        now: NOW,
        today: date(2026, 11, 17),
        archived: &archived,
        protected,
    };
    evaluate(&inputs, rows)
}

fn paths(found: &[Candidate], f: &Fixture) -> Vec<String> {
    found
        .iter()
        .map(|c| {
            c.path
                .strip_prefix(&f.root)
                .unwrap()
                .to_string_lossy()
                .into_owned()
        })
        .collect()
}

fn standard_rows(f: &Fixture) -> Vec<DirRow> {
    let mut root = f.row("", 10_000, 600, 1);
    root.role = Some(Role::Root);
    let mut ended = f.row("ended", 3000, 300, 400);
    ended.role = Some(Role::Grant);
    let mut active = f.row("active", 5000, 200, 1);
    active.role = Some(Role::Grant);
    vec![
        root,
        ended,
        f.row("ended/sub", 1000, 100, 400),
        active,
        f.row("active/runs", 4000, 150, 300),
        f.row("active/runs/r1", 2000, 100, 300),
        f.row("active/.hidden", 500, 40, 500),
        f.row("loose", 2000, 100, 900),
    ]
}

#[test]
fn ended_grants_propose_their_directories() {
    let f = Fixture::new();
    let cfg = f.config(
        r#"
        [[policy]]
        name = "ended"
        match = { grant_ended_for = "90d" }
        "#,
    );
    let found = run(&cfg, &standard_rows(&f), &[], &[]);
    assert_eq!(paths(&found, &f), ["ended"]);
    assert_eq!(found[0].grant.as_deref(), Some("OLD"));
    assert_eq!(found[0].reason, "grant OLD ended 2025-06-30");

    // Not yet 600 days since the end.
    let cfg = f.config(
        r#"
        [[policy]]
        name = "ended"
        match = { grant_ended_for = "600d" }
        "#,
    );
    assert!(run(&cfg, &standard_rows(&f), &[], &[]).is_empty());
}

#[test]
fn unchanged_data_inside_grants_only_unless_a_path_is_named() {
    let f = Fixture::new();
    let cfg = f.config(
        r#"
        [[policy]]
        name = "stale"
        match = { unchanged_for = "180d" }
        "#,
    );
    // The ended grant and active/runs, outermost only; not the hidden
    // directory, not the unassigned one, not roots.
    let found = run(&cfg, &standard_rows(&f), &[], &[]);
    assert_eq!(paths(&found, &f), ["active/runs", "ended"]);
    assert!(
        found[0].reason.starts_with("unchanged for 10 months"),
        "{}",
        found[0].reason
    );

    let cfg = f.config(
        r#"
        [[policy]]
        name = "loose"
        match = { path = "ROOT/lo*", unchanged_for = "1y" }
        "#
        .replace("ROOT", f.root.to_str().unwrap())
        .as_str(),
    );
    assert_eq!(
        paths(&run(&cfg, &standard_rows(&f), &[], &[]), &f),
        ["loose"]
    );
}

#[test]
fn size_conditions_pick_the_outermost_matching_directory() {
    let f = Fixture::new();
    let cfg = f.config(
        r#"
        [[policy]]
        name = "small-files"
        match = { unchanged_for = "180d", min_files = 120, max_avg_file_size = "25B" }
        "#,
    );
    // active/runs: 150 files averaging 26 B fails; r1 (100 files) has too few;
    // ended: 300 files averaging 10 B matches.
    assert_eq!(
        paths(&run(&cfg, &standard_rows(&f), &[], &[]), &f),
        ["ended"]
    );

    let cfg = f.config(
        r#"
        [[policy]]
        name = "big"
        match = { grant = "NEW", min_bytes = "1KB" }
        "#,
    );
    assert_eq!(
        paths(&run(&cfg, &standard_rows(&f), &[], &[]), &f),
        ["active"]
    );
}

#[test]
fn excluded_directories_stay_and_are_not_counted() {
    let f = Fixture::new();
    let cfg = f.config(
        r#"
        [[policy]]
        name = "ended"
        match = { grant_ended_for = "1d", min_bytes = "2500B" }
        "#,
    );
    let mut rows = standard_rows(&f);
    let mut env = f.row("ended/env", 1000, 50, 2);
    env.excluded = Some(ExcludeKind::Environment);
    rows.push(env);
    // ended holds 3000 bytes, 1000 of them in an environment: too small now.
    assert!(run(&cfg, &rows, &[], &[]).is_empty());

    let cfg = f.config(
        r#"
        [[policy]]
        name = "ended"
        match = { grant_ended_for = "1d" }
        "#,
    );
    let found = run(&cfg, &rows, &[], &[]);
    assert_eq!(
        (found[0].bytes, found[0].files, found[0].kept_bytes),
        (2000, 250, 1000)
    );
}

#[test]
fn archived_protected_and_multi_grant_directories_are_skipped() {
    let f = Fixture::new();
    let cfg = f.config(
        r#"
        [[policy]]
        name = "everything"
        match = { path = "ROOT/**", unchanged_for = "1d" }
        "#
        .replace("ROOT", f.root.to_str().unwrap())
        .as_str(),
    );
    let rows = vec![
        f.row("ended", 10, 1, 400),
        f.row("ended/x/.stomp-archive/A", 10, 1, 400),
        f.row("loose", 10, 1, 400),
        f.row("both", 20, 2, 400),
        f.row("both/a", 10, 1, 400),
    ];
    // "both" contains no grant, but "ended" is archived and "loose" is protected.
    let found = run(&cfg, &rows, &[f.p("ended")], &[f.p("loose/state")]);
    assert_eq!(paths(&found, &f), ["both"]);

    // A directory holding a grant's directory can't be archived as a whole.
    let rows = vec![f.row("", 10, 1, 400), f.row("ended", 10, 1, 400)];
    let found = run(&cfg, &rows, &[], &[]);
    assert_eq!(paths(&found, &f), ["ended"]);
}

#[test]
fn outer_proposals_win_across_policies() {
    let f = Fixture::new();
    let cfg = f.config(
        r#"
        [[policy]]
        name = "runs"
        match = { grant = "NEW", unchanged_for = "200d" }
        destination = "local"
        [[policy]]
        name = "whole-grant"
        match = { grant = "NEW", min_bytes = "4KB" }
        "#,
    );
    let found = run(&cfg, &standard_rows(&f), &[], &[]);
    assert_eq!(paths(&found, &f), ["active"]);
    assert_eq!(found[0].policy, "whole-grant");
}

#[test]
fn config_rejects_unsafe_or_empty_policies() {
    let f = Fixture::new();
    let text = |policy: &str| {
        format!(
            r#"
            general.roots = ["ROOT"]
            [[grant]]
            id = "G"
            dirs = ["ROOT/ended"]
            {policy}
            "#
        )
        .replace("ROOT", f.root.to_str().unwrap())
    };
    let problems = |policy: &str| {
        Config::parse(&text(policy), Path::new("t.toml"))
            .unwrap_err()
            .problems
            .join("\n")
    };
    assert!(problems("[[policy]]\nname = \"p\"").contains("has no conditions"));
    assert!(
        problems("[[policy]]\nname = \"p\"\naction = \"apply\"\nmatch = { grant = \"G\" }")
            .contains("needs match.grant_ended_for or match.unchanged_for")
    );
    assert!(
        problems("[[policy]]\nname = \"p\"\nmatch = { grant = \"X\" }").contains("no grant \"X\"")
    );
    assert!(
        problems("[[policy]]\nname = \"p\"\nmatch = { older = \"1d\" }").contains("unknown field")
    );
    assert!(
        problems("[[policy]]\nname = \"p\"\nmatch = { path = \"runs/*\" }")
            .contains("must be an absolute path")
    );
    let dup = "[[policy]]\nname = \"p\"\nmatch = { grant = \"G\" }\n";
    assert!(problems(&format!("{dup}{dup}")).contains("defined more than once"));

    let cfg = Config::parse(
        &text(
            "[[policy]]\nname = \"p\"\naction = \"apply\"\nwait = \"2d\"\nmatch = { grant = [\"G\"], unchanged_for = \"1y\" }",
        ),
        Path::new("t.toml"),
    )
    .unwrap();
    let p = &cfg.policies[0];
    assert_eq!(p.action, PolicyAction::Apply);
    assert_eq!(p.wait.as_secs(), 2 * 86_400);
    assert_eq!(p.destination, "local");
    assert_eq!(p.conditions.grants, ["G"]);
}

#[test]
fn estimates_compression_from_samples() {
    let f = Fixture::new();
    let text = f.p("active/runs");
    let noise = f.p("ended");
    std::fs::write(text.join("a.csv"), "site,value\n".repeat(50_000)).unwrap();
    let mut x: u64 = 1;
    let random: Vec<u8> = (0..300_000)
        .map(|_| {
            x ^= x << 13;
            x ^= x >> 7;
            x ^= x << 17;
            x as u8
        })
        .collect();
    std::fs::write(noise.join("b.bin"), &random).unwrap();
    let rows = vec![
        f.row("active/runs", 550_000, 1, 1),
        f.row("ended", 300_000, 1, 1),
    ];

    let ratio = estimate_ratio(&text, &rows, 3).unwrap();
    assert!(ratio < 0.05, "text compressed to {ratio}");
    let ratio = estimate_ratio(&noise, &rows, 3).unwrap();
    assert!(ratio > 0.95, "random data compressed to {ratio}");
    assert!(estimate_ratio(&f.p("loose"), &rows, 3).is_none());
}
