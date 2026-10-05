//! Automatic sync: BoilR waits in the tray, notices when the games to import changed and
//! imports them, never restarting Steam under a running game. Only Windows starts it;
//! elsewhere the code is compiled and unit-tested but never run, hence the allow.
#![cfg_attr(not(windows), allow(dead_code))]

pub mod detect;
pub mod executor;
pub mod machine;
pub mod probes;
#[cfg(windows)]
mod probes_windows;
