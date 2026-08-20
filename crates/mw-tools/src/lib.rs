//! Shared strict browser-to-native checkpoint adapter.
//!
//! The `mw-tools` binary and native viewer intentionally use this same parser
//! and runtime builder so production checkpoint validation cannot drift.

pub mod native_runtime;
