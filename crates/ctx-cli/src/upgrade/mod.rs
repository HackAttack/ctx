mod command;
mod config;
mod diagnostics;
pub(crate) mod ports;
mod presentation;

pub use command::{run, UpgradeArgs};
pub(crate) use diagnostics::upgrade_diagnostics;
