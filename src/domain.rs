//! Pure application policy for interaction priority and foreground lifecycle.
//!
//! This module deliberately contains no device, renderer, storage, or network
//! types.  The runtime translates the returned effects at its boundary.  That
//! keeps user-input priority testable without constructing the 2,000-line
//! device loop and is the first migration seam for the future manager SDK.

/// Relative importance of work that can change the visible page.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub(crate) enum Priority {
    #[allow(dead_code)] // Reserved for storage/cleanup effects in the next slice.
    Maintenance,
    AutomaticOutput,
    UserInput,
    #[allow(dead_code)] // Exercised by the pure policy contract and state gate tests.
    RequestedOutput,
    #[allow(dead_code)] // Reserved for manager-forced lifecycle transitions.
    Lifecycle,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Lifecycle {
    Foreground,
    Background,
}

/// The policy state needed to decide whether work may reach the page.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Model {
    pub(crate) lifecycle: Lifecycle,
    pub(crate) active_output: Option<Priority>,
    pub(crate) user_input_active: bool,
}

impl Model {
    pub(crate) fn foreground() -> Self {
        Self {
            lifecycle: Lifecycle::Foreground,
            active_output: None,
            user_input_active: false,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum AppEvent {
    #[allow(dead_code)] // Runtime currently starts already foregrounded.
    EnterForeground,
    EnterBackground,
    #[allow(dead_code)] // Runtime transitions currently enforce this at the input gate.
    OutputStarted {
        priority: Priority,
    },
    #[allow(dead_code)] // Full output-state migration will emit this directly.
    OutputFinished,
    UserInputStarted,
    UserInputFinished,
    #[allow(dead_code)] // Scheduler routing migrates in the next slice.
    AutomaticOutputReady,
}

/// Commands for the impure runtime boundary.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Effect {
    AcquireForeground,
    ReleaseForeground,
    CancelOutput { priority: Priority },
    ClearTransientOutput,
    StartAutomaticOutput,
    DeferAutomaticOutput,
}

/// Apply one event deterministically and return work for the runtime boundary.
pub(crate) fn reduce(model: &mut Model, event: AppEvent) -> Vec<Effect> {
    match event {
        AppEvent::EnterForeground => {
            if model.lifecycle == Lifecycle::Foreground {
                return Vec::new();
            }
            model.lifecycle = Lifecycle::Foreground;
            vec![Effect::AcquireForeground]
        }
        AppEvent::EnterBackground => {
            if model.lifecycle == Lifecycle::Background {
                return Vec::new();
            }
            let mut effects = Vec::new();
            if let Some(priority) = model.active_output.take() {
                effects.push(Effect::CancelOutput { priority });
                effects.push(Effect::ClearTransientOutput);
            }
            model.user_input_active = false;
            model.lifecycle = Lifecycle::Background;
            effects.push(Effect::ReleaseForeground);
            effects
        }
        AppEvent::OutputStarted { priority } => {
            if model.lifecycle == Lifecycle::Foreground && !model.user_input_active {
                model.active_output = Some(priority);
            }
            Vec::new()
        }
        AppEvent::OutputFinished => {
            model.active_output = None;
            Vec::new()
        }
        AppEvent::UserInputStarted => {
            model.user_input_active = true;
            let Some(priority) = model.active_output else {
                return Vec::new();
            };
            // A requested answer owns the paper until its animation reaches
            // AnswerVisible; a contact then starts the explicit fade state.
            // Only automatic heartbeat output is preempted immediately.
            if priority != Priority::AutomaticOutput {
                return Vec::new();
            }
            model.active_output = None;
            debug_assert!(Priority::UserInput > priority);
            vec![
                Effect::CancelOutput { priority },
                Effect::ClearTransientOutput,
            ]
        }
        AppEvent::UserInputFinished => {
            model.user_input_active = false;
            Vec::new()
        }
        AppEvent::AutomaticOutputReady => {
            if model.lifecycle == Lifecycle::Foreground && !model.user_input_active {
                model.active_output = Some(Priority::AutomaticOutput);
                vec![Effect::StartAutomaticOutput]
            } else {
                vec![Effect::DeferAutomaticOutput]
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn requested_output_keeps_the_page_until_its_explicit_fade_state() {
        let mut model = Model::foreground();
        reduce(
            &mut model,
            AppEvent::OutputStarted {
                priority: Priority::RequestedOutput,
            },
        );

        assert!(reduce(&mut model, AppEvent::UserInputStarted).is_empty());
        assert!(model.user_input_active);
        assert_eq!(model.active_output, Some(Priority::RequestedOutput));
        assert!(Priority::RequestedOutput > Priority::UserInput);
    }

    #[test]
    fn automatic_output_is_deferred_during_physical_input() {
        let mut model = Model::foreground();
        reduce(&mut model, AppEvent::UserInputStarted);

        assert_eq!(
            reduce(&mut model, AppEvent::AutomaticOutputReady),
            vec![Effect::DeferAutomaticOutput]
        );
        assert_eq!(model.active_output, None);

        reduce(&mut model, AppEvent::UserInputFinished);
        assert_eq!(
            reduce(&mut model, AppEvent::AutomaticOutputReady),
            vec![Effect::StartAutomaticOutput]
        );
        assert_eq!(model.active_output, Some(Priority::AutomaticOutput));
    }

    #[test]
    fn user_input_preempts_only_automatic_output() {
        let mut model = Model::foreground();
        reduce(
            &mut model,
            AppEvent::OutputStarted {
                priority: Priority::AutomaticOutput,
            },
        );
        assert_eq!(
            reduce(&mut model, AppEvent::UserInputStarted),
            vec![
                Effect::CancelOutput {
                    priority: Priority::AutomaticOutput,
                },
                Effect::ClearTransientOutput,
            ]
        );
        assert_eq!(model.active_output, None);
    }

    #[test]
    fn background_cancels_output_and_blocks_automatic_work_until_foreground() {
        let mut model = Model::foreground();
        reduce(
            &mut model,
            AppEvent::OutputStarted {
                priority: Priority::AutomaticOutput,
            },
        );

        assert_eq!(
            reduce(&mut model, AppEvent::EnterBackground),
            vec![
                Effect::CancelOutput {
                    priority: Priority::AutomaticOutput,
                },
                Effect::ClearTransientOutput,
                Effect::ReleaseForeground,
            ]
        );
        assert_eq!(model.lifecycle, Lifecycle::Background);
        assert_eq!(
            reduce(&mut model, AppEvent::AutomaticOutputReady),
            vec![Effect::DeferAutomaticOutput]
        );
        assert_eq!(
            reduce(&mut model, AppEvent::EnterForeground),
            vec![Effect::AcquireForeground]
        );
        assert_eq!(model.lifecycle, Lifecycle::Foreground);
    }

    #[test]
    fn lifecycle_events_are_idempotent() {
        let mut model = Model::foreground();
        assert!(reduce(&mut model, AppEvent::EnterForeground).is_empty());
        assert_eq!(
            reduce(&mut model, AppEvent::EnterBackground),
            vec![Effect::ReleaseForeground]
        );
        assert!(reduce(&mut model, AppEvent::EnterBackground).is_empty());
    }

    #[test]
    fn priorities_form_the_expected_preemption_order() {
        assert!(Priority::Lifecycle > Priority::UserInput);
        assert!(Priority::RequestedOutput > Priority::UserInput);
        assert!(Priority::UserInput > Priority::AutomaticOutput);
        assert!(Priority::AutomaticOutput > Priority::Maintenance);
    }
}
