//! stomp: storage and compute tracking per research grant for HPC users.
//!
//! The design lives in `spec/design.md`; module boundaries follow §20.1 of it.

pub mod attribution;
pub mod catalog;
pub mod cli;
pub mod cmd;
pub mod config;
pub mod fs;
pub mod jobs;
pub mod paths;
pub mod sys;
pub mod table;
pub mod units;
