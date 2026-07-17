//! Command-line entry and headless diagnostics.

use std::sync::mpsc;
use std::time::Instant;

use crate::oracle::Event;
use crate::{memory, oracle, power, tasks, todos};

use super::context::build_ctx;
use super::runtime::{run, PNG_PATH};

const USAGE: &str = "\
MagicPaper (MP) — your living magical paper

usage:
  riddle                      open the diary (windowed when AppLoad sets
                              QTFB_KEY, otherwise takeover via libquill)
  riddle --oracle-test [PNG]  run one oracle turn against PNG (default
                              /tmp/riddle-page.png) and print the streamed
                              reply; verifies key + endpoint + model
  riddle --ocr-test PNG       send one PNG only to the configured PaddleOCR
                              service and print its recognized text
  riddle --power-launcher     watch for three quick power-button presses and
                              launch the standalone diary
  riddle --version            print the version

standalone configuration lives in /home/root/.config/riddle/oracle.env.
";

pub(crate) fn entry() {
    let args: Vec<String> = std::env::args().collect();
    match args.get(1).map(String::as_str) {
        // Diagnostic: run one oracle turn and print the streamed chunks.
        // Lets you verify your endpoint + key + model before ever launching
        // the diary. No display needed.
        Some("--oracle-test") => {
            let png = args.get(2).map(String::as_str).unwrap_or(PNG_PATH);
            std::process::exit(oracle_test(png));
        }
        Some("--ocr-test") => {
            let Some(png) = args.get(2) else {
                eprintln!("riddle: --ocr-test needs a PNG path");
                std::process::exit(2);
            };
            match oracle::paddle_ocr_test(png) {
                Ok(text) => println!("{text}"),
                Err(error) => {
                    eprintln!("OCR test failed: {error}");
                    std::process::exit(1);
                }
            }
            return;
        }
        Some("--power-launcher") => {
            if let Err(e) = power::launcher_loop() {
                eprintln!("riddle: power launcher fatal: {e}");
                std::process::exit(1);
            }
            return;
        }
        Some("--version" | "-V") => {
            println!("MagicPaper (MP) {}", env!("CARGO_PKG_VERSION"));
            return;
        }
        Some("--help" | "-h") => {
            print!("{USAGE}");
            return;
        }
        Some(flag) if flag.starts_with('-') => {
            eprintln!("riddle: unknown flag {flag}\n");
            eprint!("{USAGE}");
            std::process::exit(2);
        }
        _ => {}
    }
    if let Err(e) = run() {
        eprintln!("riddle: fatal: {e}");
        std::process::exit(1);
    }
}

fn oracle_test(png: &str) -> i32 {
    let store = memory::MemoryStore::open();
    let task_store = tasks::TaskStore::open();
    let todo_store = todos::TodoStore::open();
    let o = match oracle::Oracle::spawn(
        store.is_some() || task_store.is_some() || todo_store.is_some(),
    ) {
        Ok(o) => o,
        Err(e) => {
            eprintln!("oracle spawn failed: {e}");
            return 1;
        }
    };
    let ctx = build_ctx(&store, &task_store, &todo_store);
    let (tx, rx) = mpsc::channel();
    let t0 = Instant::now();
    o.ask(png, &ctx, tx);
    let mut got = String::new();
    loop {
        match rx.recv() {
            Ok(Ok(Event::Ink(chunk))) => {
                if got.is_empty() {
                    eprintln!("first chunk +{}ms", t0.elapsed().as_millis());
                }
                print!("{chunk} ");
                use std::io::Write as _;
                let _ = std::io::stdout().flush();
                got.push_str(&chunk);
            }
            Ok(Ok(Event::Show(id))) => {
                println!("[would conjure memory {id} — {}]", memory::spoken_date(id));
                got.push_str("(show)");
            }
            Ok(Ok(Event::TaskList)) => {
                println!("[would open recurring-task list]");
                got.push_str("(tasks)");
            }
            Ok(Ok(Event::TodoList)) => {
                println!("[would open TODO list]");
                got.push_str("(todos)");
            }
            Ok(Ok(Event::FontList)) => {
                println!("[would open font list]");
                got.push_str("(fonts)");
            }
            Ok(Ok(Event::HistoryList)) => {
                println!("[would open history list]");
                got.push_str("(history)");
            }
            Ok(Ok(Event::Help)) => {
                println!("[would open instruction manual]");
                got.push_str("(help)");
            }
            Ok(Ok(Event::LocalCommand(command))) => {
                println!("[would apply local command: {command}]");
                got.push_str("(local-command)");
            }
            Ok(Ok(Event::Transcript(t))) => eprintln!("\n[transcript] {t}"),
            Ok(Err(e)) => {
                eprintln!("\noracle error: {e}");
                return 1;
            }
            Err(_) => break, // disconnected = reply complete
        }
    }
    println!(
        "\n--- reply complete ({}ms, {} chars) ---",
        t0.elapsed().as_millis(),
        got.len()
    );
    if got.trim().is_empty() {
        1
    } else {
        0
    }
}
