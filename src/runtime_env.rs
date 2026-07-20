//! Shared detection of the Remagic-hosted runtime.
//!
//! Several generations of the launcher used different environment names.
//! Keep the compatibility policy in one place so input, power, heartbeat and
//! handoff code cannot disagree about who owns the device lifecycle.

const MANAGED_VARS: [&str; 3] = [
    "REMAGIC_RUNTIME_MANAGED",
    "REMAGIC_MANAGED",
    "RIDDLE_SYSTEMD_MANAGED",
];

/// True when MagicPaper is running as an application owned by Remagic.
pub fn is_managed() -> bool {
    MANAGED_VARS.iter().any(|name| {
        std::env::var(name)
            .ok()
            .is_some_and(|value| env_flag_enabled(&value))
    })
}

fn env_flag_enabled(value: &str) -> bool {
    !matches!(
        value.trim().to_ascii_lowercase().as_str(),
        "" | "0" | "false" | "no" | "off"
    )
}

#[cfg(test)]
mod tests {
    use super::env_flag_enabled;

    #[test]
    fn managed_flags_reject_explicit_false_values() {
        for value in ["", " ", "0", "false", "FALSE", "no", "off"] {
            assert!(!env_flag_enabled(value), "{value:?}");
        }
        for value in ["1", "true", "yes", "managed"] {
            assert!(env_flag_enabled(value), "{value:?}");
        }
    }
}
