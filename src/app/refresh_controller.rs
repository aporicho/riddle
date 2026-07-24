//! One policy boundary for partial cleanup and full-panel refresh debt.

use crate::display::Display;
use crate::fb::BBox;
use crate::platform::RefreshIntent;
use crate::preferences::{CleanupStrength, PreferenceValues, UserPreferences};

pub(super) struct RefreshController {
    preferences: UserPreferences,
    replies_since_full: u8,
}

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

    /// Settle one completed reply. Returns true when this completion used a
    /// full-panel refresh instead of a partial cleanup.
    pub(super) fn present_reply_cleanup(
        &mut self,
        display: &Display,
        surface_width: usize,
        surface_height: usize,
        region: BBox,
    ) -> bool {
        if self.reply_cleanup_is_full() {
            self.request_full(display, surface_width, surface_height);
            return true;
        }
        self.present_cleanup(display, region);
        false
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
}
