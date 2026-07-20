//! MagicPaper (MP) — living paper for the reMarkable Paper Pro Move.

mod agent;
mod app;
mod appearance;
mod display;
mod fb;
mod ink;
mod oracle;
mod pen;
mod power;
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
