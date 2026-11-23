//! `stomp archive --estimate` (§12.3): predicts compressed size and time by
//! compressing a sample of the files.

use std::fs::File;
use std::io::Read;
use std::path::Path;
use std::time::Instant;

use anyhow::Result;
use serde::Serialize;

use super::manifest::Kind;
use super::plan::Plan;

/// Files sampled, chosen in proportion to their size.
const SAMPLES: u64 = 64;
/// Bytes read from the start of each sampled file.
const SAMPLE_BYTES: usize = 4 << 20;

#[derive(Debug, Clone, Serialize)]
pub struct Estimate {
    pub sampled_files: usize,
    pub sampled_bytes: u64,
    /// Compressed size as a fraction of the original.
    pub ratio: f64,
    pub predicted_bytes: u64,
    /// Compression speed of one thread, in MB/s.
    pub mb_per_second: f64,
    pub threads: u32,
    /// Compression time alone; reading the files may well take longer.
    pub predicted_seconds: u64,
    pub files_before: u64,
    /// Chunks plus the manifest, checksums, README, journal and note.
    pub files_after: u64,
}

pub fn estimate(source: &Path, plan: &Plan, level: i32, threads: u32) -> Result<Estimate> {
    let files: Vec<_> = plan
        .entries
        .iter()
        .filter(|e| e.kind == Kind::File && e.size > 0)
        .collect();
    let total: u64 = files.iter().map(|e| e.size).sum();

    // Systematic sampling along cumulative size: big files are more likely to
    // be picked, as they matter more to the result.
    let mut picks = Vec::new();
    if total > 0 {
        let n = SAMPLES.min(files.len() as u64);
        let step = total / n;
        let (mut cumulative, mut next) = (0u64, step / 2);
        for (i, e) in files.iter().enumerate() {
            cumulative += e.size;
            if cumulative > next {
                picks.push(i);
                while next < cumulative {
                    next += step.max(1);
                }
            }
        }
    }

    let (mut read, mut compressed, mut seconds) = (0u64, 0u64, 0f64);
    let mut buf = vec![0u8; SAMPLE_BYTES];
    for &i in &picks {
        let Ok(mut file) = File::open(source.join(&files[i].rel)) else {
            continue;
        };
        let mut n = 0;
        while n < buf.len() {
            match file.read(&mut buf[n..]) {
                Ok(0) | Err(_) => break,
                Ok(m) => n += m,
            }
        }
        if n == 0 {
            continue;
        }
        let started = Instant::now();
        let out = zstd::bulk::compress(&buf[..n], level)?;
        seconds += started.elapsed().as_secs_f64();
        read += n as u64;
        compressed += out.len() as u64;
    }
    let ratio = if read > 0 {
        compressed as f64 / read as f64
    } else {
        1.0
    };
    let mb_per_second = if seconds > 0.0 {
        read as f64 / 1e6 / seconds
    } else {
        0.0
    };
    // Each tar entry adds a 512-byte header, which compresses well.
    let entries = plan.entries.len() as u64;
    let predicted_bytes = (total as f64 * ratio) as u64 + entries * 64;
    let parallel = f64::from(threads.clamp(1, 16));
    let predicted_seconds = if mb_per_second > 0.0 {
        (total as f64 / 1e6 / (mb_per_second * parallel)) as u64
    } else {
        0
    };
    Ok(Estimate {
        sampled_files: picks.len(),
        sampled_bytes: read,
        ratio,
        predicted_bytes,
        mb_per_second,
        threads,
        predicted_seconds,
        files_before: plan.summary.files + plan.summary.symlinks + plan.summary.hardlinks,
        // Chunks stay in the directory for a local archive; plan_only corrects a remote one.
        files_after: u64::from(plan.summary.chunks) + super::RECORD_FILES,
    })
}
