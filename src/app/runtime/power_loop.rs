use std::time::{Duration, Instant};

use super::super::timing::heartbeat_deadline;
use super::Engine;
use crate::{power, ui};

impl Engine<'_> {
    /// Returns true when the triple-click requests runtime exit.
    pub(super) fn handle_power(&mut self) -> bool {
        let Some(button) = self.power_dev.as_mut() else {
            return false;
        };
        let now = Instant::now();
        let presses = button.drain_press_count();
        let action = if now >= self.power_grace {
            self.power_clicks.push(presses, now)
        } else {
            if presses > 0 {
                self.power_clicks.clear();
            }
            power::ClickAction::None
        };
        match action {
            power::ClickAction::Triple => {
                eprintln!("riddle: triple power quit");
                button.wait_for_release(Duration::from_millis(700));
                true
            }
            power::ClickAction::Single => {
                self.sleep_and_wake();
                false
            }
            power::ClickAction::None => false,
        }
    }

    fn sleep_and_wake(&mut self) {
        eprintln!("riddle: sleeping (power button)");
        let saved = ui::help::show_sleep(&mut self.surf, &self.font);
        self.refresh
            .request_full(self.disp, self.surf.w, self.surf.h);
        std::thread::sleep(Duration::from_millis(800));
        if let Some(button) = self.power_dev.as_mut() {
            suspend_until_success(button);
        }
        eprintln!("riddle: waking");
        ui::help::restore_sleep(&mut self.surf, &saved);
        self.refresh
            .request_full(self.disp, self.surf.w, self.surf.h);
        power::wifi_heal();
        self.discard_sleep_input();
        self.power_clicks.clear();
        self.power_grace = Instant::now() + Duration::from_secs(3);
        self.next_heartbeat = if self.agent_queue_mode {
            None
        } else {
            heartbeat_deadline(&self.task_store)
        };
    }

    fn discard_sleep_input(&mut self) {
        if let Some(device) = self.pen_dev.as_mut() {
            let _ = device.drain();
        }
        if let Some(device) = self.touch_dev.as_mut() {
            let _ = device.drain_check_quit();
        }
        if let Some(button) = self.power_dev.as_mut() {
            button.drain_pressed();
        }
    }
}

fn suspend_until_success(button: &mut power::PowerButton) {
    let initial_count = power::suspend_count();
    for attempt in 1..=8 {
        if button.grabbed {
            let _ = std::process::Command::new("systemctl")
                .arg("suspend")
                .status();
        }
        let started = Instant::now();
        while started.elapsed() < Duration::from_secs(6) {
            std::thread::sleep(Duration::from_millis(400));
            if power::suspend_count() > initial_count {
                return;
            }
        }
        if attempt == 8 {
            eprintln!("riddle: suspend never happened ({attempt} tries); waking the page");
            return;
        }
        eprintln!("riddle: suspend aborted (EPD discharge timer), retrying");
    }
}
