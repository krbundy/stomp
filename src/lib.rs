//! stomp: storage and compute tracking per research grant for HPC users.
//!
//! The design lives in `spec/design.md`; module boundaries follow §20.1 of it.

pub mod archive;
pub mod attribution;
pub mod catalog;
pub mod cli;
pub mod cmd;
pub mod compute;
pub mod config;
pub mod dest;
pub mod fs;
pub mod jobs;
pub mod paths;
pub mod scan;
pub mod sys;
pub mod table;
pub mod tasks;
pub mod units;
