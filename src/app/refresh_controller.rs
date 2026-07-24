//! One policy boundary for partial cleanup and full-panel refresh debt.

use crate::display::Display;
use crate::fb::BBox;
use crate::platform::RefreshIntent;
use crate::preferences::{CleanupStrength, PreferenceValues, UserPreferences};

pub(super) struct RefreshController {
    preferences: UserPreferences,
    replies_since_full: u8,
}

pub(super) const REPLY_FADE_STAGES: u32 = 10;

impl RefreshController {
    pub(super) fn open() -> Self {
        Self {
            preferences: UserPreferences::open(),
            replies_since_full: 0,
        }
    }

    pub(super) const fn values(&self) -> PreferenceValues {
        self.preferences.values()
    }

    pub(super) fn replace_values(&mut self, values: PreferenceValues) -> std::io::Result<()> {
        let interval_changed =
            self.values().full_refresh_every_replies != values.full_refresh_every_replies;
        let result = self.preferences.replace(values);
        if interval_changed {
            self.replies_since_full = 0;
        }
        result
    }

    pub(super) fn request_full(&mut self, display: &Display, width: usize, height: usize) {
        display.request_refresh(width, height);
        self.reset_debt();
    }

    pub(super) fn present_cleanup(&self, display: &Display, region: BBox) {
        if region.is_empty() {
            return;
        }
        let region = region.expanded(self.values().cleanup_padding_px as i32);
        let (x, y, width, height) = region.rect();
        display.present_region(x, y, width, height, self.cleanup_intent());
    }

    /// Finish one dissolved reply with exactly one monochrome quality partial
    /// update, unless the user explicitly opted into the periodic full-panel
    /// cleanup. Returns true when that opt-in full refresh was used.
    pub(super) fn present_reply_cleanup(
        &mut self,
        display: &Display,
        surface_width: usize,
        surface_height: usize,
        region: BBox,
    ) -> bool {
        match self.next_reply_cleanup_intent() {
            RefreshIntent::Full => {
                self.request_full(display, surface_width, surface_height);
                true
            }
            RefreshIntent::MonoQuality => {
                if !region.is_empty() {
                    let region = region.expanded(self.values().cleanup_padding_px as i32);
                    let (x, y, width, height) = region.rect();
                    display.present_region(x, y, width, height, RefreshIntent::MonoQuality);
                }
                false
            }
            _ => unreachable!("reply cleanup has only monochrome or full intent"),
        }
    }

    pub(super) const fn cleanup_intent(&self) -> RefreshIntent {
        match self.preferences.values().cleanup_strength {
            CleanupStrength::Standard => RefreshIntent::MonoQuality,
            CleanupStrength::Enhanced => RefreshIntent::CleanPartial,
        }
    }

    fn reply_cleanup_is_full(&mut self) -> bool {
        let interval = self.values().full_refresh_every_replies;
        if interval == 0 {
            return false;
        }
        self.replies_since_full = self.replies_since_full.saturating_add(1);
        self.replies_since_full >= interval
    }

    fn next_reply_cleanup_intent(&mut self) -> RefreshIntent {
        if self.reply_cleanup_is_full() {
            self.reset_debt();
            RefreshIntent::Full
        } else {
            RefreshIntent::MonoQuality
        }
    }

    fn reset_debt(&mut self) {
        self.replies_since_full = 0;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_policy_uses_enhanced_partial_cleanup() {
        let values = PreferenceValues::default();
        assert_eq!(values.cleanup_strength, CleanupStrength::Enhanced);
        assert_eq!(values.cleanup_padding_px, 16);
        assert_eq!(values.full_refresh_every_replies, 0);
    }

    #[test]
    fn reply_debt_supports_disabled_every_reply_and_balanced_intervals() {
        for (interval, expected) in [
            (0, vec![false, false, false, false]),
            (1, vec![true, true, true, true]),
            (3, vec![false, false, true, false]),
        ] {
            let values = PreferenceValues {
                full_refresh_every_replies: interval,
                ..PreferenceValues::default()
            };
            let mut controller = RefreshController {
                preferences: UserPreferences::for_test(values),
                replies_since_full: 0,
            };
            let actual: Vec<bool> = (0..4)
                .map(|_| {
                    let full = controller.reply_cleanup_is_full();
                    if full {
                        controller.reset_debt();
                    }
                    full
                })
                .collect();
            assert_eq!(actual, expected, "interval={interval}");
        }
    }

    #[test]
    fn any_full_refresh_resets_accumulated_reply_debt() {
        let mut controller = RefreshController {
            preferences: UserPreferences::for_test(PreferenceValues {
                full_refresh_every_replies: 3,
                ..PreferenceValues::default()
            }),
            replies_since_full: 2,
        };
        controller.reset_debt();
        assert!(!controller.reply_cleanup_is_full());
        assert_eq!(controller.replies_since_full, 1);
    }

    #[test]
    fn fade_submission_sequence_is_nine_ink_then_one_configured_cleanup() {
        for (interval, expected_terminal) in
            [(0, RefreshIntent::MonoQuality), (1, RefreshIntent::Full)]
        {
            let mut controller = RefreshController {
                preferences: UserPreferences::for_test(PreferenceValues {
                    full_refresh_every_replies: interval,
                    ..PreferenceValues::default()
                }),
                replies_since_full: 0,
            };
            let mut submitted = vec![RefreshIntent::Ink; (REPLY_FADE_STAGES - 1) as usize];
            submitted.push(controller.next_reply_cleanup_intent());
            assert_eq!(submitted.len(), REPLY_FADE_STAGES as usize);
            assert!(submitted[..9]
                .iter()
                .all(|intent| *intent == RefreshIntent::Ink));
            assert_eq!(submitted[9], expected_terminal);
        }
    }
}
