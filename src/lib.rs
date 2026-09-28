/// Command-line interface definitions and argument parsing.
pub mod cli;
/// Application configuration management.
pub mod config;
/// Error types and result aliases for the application.
pub mod error;
/// The gale-sync profile API, a pollable source of the desired modlist.
pub mod gale_sync;
/// Logging configuration and setup.
pub mod logs;
/// Progress reporting backed by `indicatif` for the CLI.
pub mod progress;
/// Multi-source mod resolution (Thunderstore, Hexium) and the sidecar file
/// recording which source each installed mod came from.
pub mod sources;
/// Resolves which directory an invocation installs into and tracks against.
pub mod target;
#[cfg(test)]
mod test_support;
