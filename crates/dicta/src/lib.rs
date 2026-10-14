//! Resident application controller. The GUI never owns audio or credentials.
pub mod controller;
pub mod ipc;
pub mod session;
mod shortcut_state;

mod history_ops;
mod reader;
pub mod recording;
mod wav;
mod writer;
