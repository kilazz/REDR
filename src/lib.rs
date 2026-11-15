//! REDR library crate.
//!
//! The binary in `main.rs` is a thin shell around these modules; keeping them
//! in a library target makes them testable from integration tests.

pub mod config;
pub mod logger;
pub mod scanner;
pub mod sys;
pub mod view;
