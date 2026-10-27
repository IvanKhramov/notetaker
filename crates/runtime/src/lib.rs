//! Desktop distribution utilities, shared by GUI and CLI without UI/ML dependencies.
pub mod diagnostics;
pub mod paths;
#[cfg(feature = "distribution")]
pub mod report;
#[cfg(feature = "distribution")]
pub mod update;
#[cfg(windows)]
pub mod windows;
