//! Modal pointer routing and transient component feedback.

use super::{is_list, Engine};
use crate::app::input::{ModalContact, ModalPreview};
use crate::app::lists::{finish_paper_list_stroke, PaperListContext, PaperListOutcome};
use crate::app::state::State;
use crate::platform::{PenFrame, PenPhase, PenTool, RefreshIntent};
use crate::ui;
use crate::ui::pointer::{Gesture, HitRect, Point, PointerTool};

impl Engine<'_> {
    fn finish_list_gesture(&mut self, gesture: Gesture) -> bool {
        let outcome = finish_paper_list_stroke(
            &mut self.state,
            PaperListContext {
                memory_store: &mut self.store,
                task_store: &mut self.task_store,
                todo_store: &mut self.todo_store,
                next_heartbeat: &mut self.next_heartbeat,
                surf: &mut self.surf,
                font: &mut self.font,
                disp: self.disp,
                refresh: &mut self.refresh,
            },
            gesture,
        );
        match outcome {
            PaperListOutcome::OpenReader(path) => {
                self.reader_target = Some(path);
                true
            }
            PaperListOutcome::PiAction(action) => {
                use crate::app::pi_settings_controller::PiAgentAction;
                use crate::oracle::AgentControlCommand;
                let command = match action {
                    PiAgentAction::ReloadProfile => {
                        self.oracle.apply_pi_preferences(
                            crate::pi_preferences::PiPreferences::open().values(),
                        );
                        AgentControlCommand::ReloadProfile
                    }
                    // Supplying the current profile lets ReMagic verify the
                    // selected provider's credential as well as the runtime.
                    // A bare status request cannot know a never-used app's
                    // intended provider and would report a false missing key.
                    PiAgentAction::TestConnection => {
                        self.oracle.apply_pi_preferences(
                            crate::pi_preferences::PiPreferences::open().values(),
                        );
                        AgentControlCommand::ReloadProfile
                    }
                    PiAgentAction::RestartAgent => AgentControlCommand::Restart,
                    PiAgentAction::NewSession => {
                        if let Some(store) = self.store.as_mut() {
                            match store.begin_dialogue_session() {
                                Ok(floor) => eprintln!(
                                    "magic-paper: event=dialogue-boundary-created floor={floor}"
                                ),
                                Err(error) => {
                                    eprintln!(
                                        "magic-paper: could not create dialogue boundary: {error}"
                                    );
                                    crate::app::pi_settings_controller::set_status(
                                        &mut self.state,
                                        &mut self.surf,
                                        &self.font,
                                        self.disp,
                                        crate::oracle::AgentControlStatus::StorageError,
                                    );
                                    return false;
                                }
                            }
                        }
                        AgentControlCommand::NewSession
                    }
                };
                crate::app::pi_settings_controller::set_status(
                    &mut self.state,
                    &mut self.surf,
                    &self.font,
                    self.disp,
                    crate::oracle::AgentControlStatus::Starting,
                );
                self.oracle.start_agent_control(command);
                eprintln!("magic-paper: event=pi-settings-action action={action:?}");
                false
            }
            PaperListOutcome::None => false,
        }
    }

    pub(super) fn record_modal_pen(&mut self, frame: PenFrame) {
        if frame.tool != PenTool::Pen {
            return;
        }
        match frame.phase {
            PenPhase::Down => self.begin_modal_contact(PointerTool::Pen, frame.x, frame.y),
            PenPhase::Move => self.update_modal_contact(PointerTool::Pen, frame.x, frame.y),
            PenPhase::Up | PenPhase::Cancel => {}
        }
    }

    pub(super) fn begin_modal_contact(&mut self, tool: PointerTool, x: i32, y: i32) {
        if self.modal_contact.is_some() {
            return;
        }
        let point = Point::new(x, y);
        let preview = match &self.state {
            State::TaskList { panel }
            | State::TodoList { panel }
            | State::HistoryList { panel } => {
                panel.begin_preview(tool, point).map(ModalPreview::Paper)
            }
            State::ReaderList { panel, .. } => {
                panel.begin_preview(tool, point).map(ModalPreview::Paper)
            }
            State::FontList { panel, .. } => {
                panel.begin_preview(tool, point).map(ModalPreview::Font)
            }
            State::Settings { panel } => {
                panel.begin_preview(tool, point).map(ModalPreview::Settings)
            }
            State::PiSettings { panel, .. } => panel
                .begin_preview(tool, point)
                .map(ModalPreview::PiSettings),
            State::Help {
                panel: Some(panel), ..
            } => panel.begin_preview(tool, point).map(ModalPreview::Help),
            _ => None,
        };
        let mut contact = ModalContact::begin(tool, x, y);
        let damage =
            preview.and_then(|preview| contact.attach_preview(preview, &mut self.surf, &self.font));
        self.modal_contact = Some(contact);
        if let Some(rect) = damage {
            self.present_modal_preview(rect);
        }
    }

    pub(super) fn update_modal_contact(&mut self, tool: PointerTool, x: i32, y: i32) {
        let point = Point::new(x, y);
        let damage = self.modal_contact.as_mut().and_then(|contact| {
            if !contact.push(tool, x, y) {
                return None;
            }
            contact.update_preview(point, &mut self.surf, &self.font)
        });
        if let Some(rect) = damage {
            self.present_modal_preview(rect);
        }
    }

    fn present_modal_preview(&self, rect: HitRect) {
        self.disp.present_region(
            rect.x0,
            rect.y0,
            rect.width(),
            rect.height(),
            RefreshIntent::Ink,
        );
    }

    pub(super) fn finish_modal_contact(&mut self, tool: PointerTool) -> bool {
        let Some(mut contact) = self.modal_contact.take() else {
            return true;
        };
        if contact.tool != tool {
            self.modal_contact = Some(contact);
            return true;
        }
        let preview = contact.finish_preview(&mut self.surf);
        let gesture = preview
            .map(|(gesture, _)| gesture)
            .or_else(|| contact.classify());
        let Some(gesture) = gesture else {
            return true;
        };
        if is_list(&self.state) {
            return !self.finish_list_gesture(gesture);
        }
        self.finish_help_gesture(gesture);
        true
    }

    /// Restore transient save-under pixels before another owner mutates the
    /// page during a lifecycle or non-pointer state transition.
    pub(crate) fn cancel_modal_contact(&mut self) {
        if let Some(mut contact) = self.modal_contact.take() {
            let _ = contact.finish_preview(&mut self.surf);
        }
    }

    fn finish_help_gesture(&mut self, gesture: Gesture) {
        let action = match &self.state {
            State::Help {
                panel: Some(panel), ..
            } => panel.interact(gesture),
            _ => None,
        };
        if !matches!(
            action,
            Some(ui::help::HelpAction::Close | ui::help::HelpAction::Outside)
        ) {
            return;
        }
        let old = std::mem::replace(&mut self.state, State::Listening { last_pen: None });
        if let State::Help {
            panel: Some(panel),
            until,
        } = old
        {
            let region = panel.dismiss(&mut self.surf);
            let (x, y, width, height) = region.rect();
            self.disp
                .present_region(x, y, width, height, RefreshIntent::Ui);
            self.state = State::Help { panel: None, until };
        } else {
            self.state = old;
        }
    }
}
