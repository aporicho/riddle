//! Application entry and device runtime.

mod cli;
mod context;
mod input;
mod layout_controller;
mod lifecycle;
mod lists;
mod oracle_controller;
mod reply;
mod reply_controller;
mod runtime;
mod state;
mod timing;
mod turn_controller;

pub(crate) use cli::entry;
