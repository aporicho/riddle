//! MagicPaper (MP) — living paper for reMarkable Paper Pro and Paper Pro Move.

mod agent;
mod app;
mod appearance;
mod device_profile;
mod display;
mod domain;
mod fb;
mod ink;
mod oracle;
mod pen;
mod pi_preferences;
mod platform;
mod power;
mod preferences;
mod qtfb;
mod reader;
mod runtime_control;
mod runtime_env;
mod storage;
mod surface;
mod touch;
mod ui;

pub(crate) use appearance::{fonts, script};
pub(crate) use storage::{memory, tasks, todos};

fn main() {
    app::entry();
}
