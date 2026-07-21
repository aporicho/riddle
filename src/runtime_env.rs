//! Shared detection of the Remagic-hosted runtime.
//!
//! Several generations of the launcher used different environment names.
//! Keep the compatibility policy in one place so input, power, heartbeat and
//! handoff code cannot disagree about who owns the device lifecycle.

use std::io;
use std::path::PathBuf;

// `RIDDLE_SYSTEMD_MANAGED` belongs to the old standalone takeover supervisor;
// it says nothing about the Remagic lifecycle/display contract.
const MANAGED_VARS: [&str; 2] = ["REMAGIC_RUNTIME_MANAGED", "REMAGIC_MANAGED"];

/// The normal entry is host-owned and requires qtfb. Full device takeover is
/// intentionally available only through an explicit compatibility entry.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum LaunchMode {
    Hosted,
    LegacyTakeover,
}

/// True when MagicPaper is running as an application owned by Remagic.
pub fn is_managed() -> bool {
    std::env::var_os("REMAGIC_RUNTIME_PROFILE").is_some()
        || MANAGED_VARS.iter().any(|name| {
            std::env::var(name)
                .ok()
                .is_some_and(|value| env_flag_enabled(&value))
        })
}

/// Explicit deterministic-device-test mode. Only the exact value `1` enables
/// it, so a stale or misspelled service variable cannot silently disable real
/// integrations in production.
pub fn test_mode() -> bool {
    std::env::var("RIDDLE_TEST_MODE").as_deref() == Ok("1")
}

pub fn require_external_integrations(component: &str) -> io::Result<()> {
    if test_mode() {
        Err(io::Error::new(
            io::ErrorKind::PermissionDenied,
            format!("{component} is disabled in RIDDLE_TEST_MODE"),
        ))
    } else {
        Ok(())
    }
}

/// Resolve one durable path. A component-specific override wins, followed by
/// `RIDDLE_DATA_DIR/<child>`. Test mode never falls back to `/home/root`; an
/// unconfigured run is isolated under the process temp directory instead.
pub fn persistent_path(override_var: &str, child: &str, production: &str) -> PathBuf {
    choose_persistent_path(
        std::env::var_os(override_var).map(PathBuf::from),
        std::env::var_os("RIDDLE_DATA_DIR").map(PathBuf::from),
        test_mode(),
        child,
        production,
    )
}

fn choose_persistent_path(
    direct: Option<PathBuf>,
    root: Option<PathBuf>,
    test: bool,
    child: &str,
    production: &str,
) -> PathBuf {
    if let Some(path) = direct.filter(|path| !path.as_os_str().is_empty()) {
        return path;
    }
    if let Some(root) = root.filter(|path| !path.as_os_str().is_empty()) {
        return root.join(child);
    }
    if test {
        return std::env::temp_dir()
            .join(format!(
                "magicpaper-test-unconfigured-{}",
                std::process::id()
            ))
            .join(child);
    }
    PathBuf::from(production)
}

/// Validate the fail-closed half of the manager/application contract before
/// opening display or raw input devices. Connection-level lifecycle checks
/// remain in `LifecycleClient::discover` where the inherited fd can be
/// duplicated and inspected safely.
pub(crate) fn validate_launch(mode: LaunchMode) -> io::Result<bool> {
    let managed = is_managed();
    let profile = std::env::var("REMAGIC_RUNTIME_PROFILE").ok();
    let qtfb_key = std::env::var("QTFB_KEY").ok();
    let lifecycle = std::env::var("REMAGIC_LIFECYCLE_FD")
        .ok()
        .filter(|value| !value.trim().is_empty())
        .or_else(|| {
            std::env::var("REMAGIC_LIFECYCLE_SOCKET")
                .ok()
                .filter(|value| !value.trim().is_empty())
        });
    validate_values(
        managed,
        mode,
        profile.as_deref(),
        qtfb_key.as_deref(),
        lifecycle.as_deref(),
    )?;
    Ok(managed)
}

fn validate_values(
    managed: bool,
    mode: LaunchMode,
    profile: Option<&str>,
    qtfb_key: Option<&str>,
    lifecycle: Option<&str>,
) -> io::Result<()> {
    if mode == LaunchMode::LegacyTakeover {
        if managed {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "managed MagicPaper cannot use legacy takeover",
            ));
        }
        return Ok(());
    }
    if !managed {
        return Ok(());
    }
    if profile != Some("qtfb_compat") {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "managed MagicPaper requires REMAGIC_RUNTIME_PROFILE=qtfb_compat",
        ));
    }
    if qtfb_key
        .and_then(|value| value.trim().parse::<i32>().ok())
        .is_none_or(|value| value <= 0)
    {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "managed MagicPaper requires a positive numeric QTFB_KEY",
        ));
    }
    if lifecycle.is_none() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "managed MagicPaper requires a lifecycle channel",
        ));
    }
    Ok(())
}

fn env_flag_enabled(value: &str) -> bool {
    !matches!(
        value.trim().to_ascii_lowercase().as_str(),
        "" | "0" | "false" | "no" | "off"
    )
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use super::{
        choose_persistent_path, env_flag_enabled, validate_values, LaunchMode, MANAGED_VARS,
    };

    #[test]
    fn managed_flags_reject_explicit_false_values() {
        for value in ["", " ", "0", "false", "FALSE", "no", "off"] {
            assert!(!env_flag_enabled(value), "{value:?}");
        }
        for value in ["1", "true", "yes", "managed"] {
            assert!(env_flag_enabled(value), "{value:?}");
        }
    }

    #[test]
    fn managed_launch_requires_complete_qtfb_lifecycle_contract() {
        assert!(validate_values(
            true,
            LaunchMode::Hosted,
            Some("qtfb_compat"),
            Some("245209900"),
            Some("7"),
        )
        .is_ok());
        for (profile, key, lifecycle) in [
            (Some("native_v2"), Some("7"), Some("8")),
            (Some("qtfb_compat"), None, Some("8")),
            (Some("qtfb_compat"), Some("0"), Some("8")),
            (Some("qtfb_compat"), Some("not-a-key"), Some("8")),
            (Some("qtfb_compat"), Some("7"), None),
        ] {
            assert!(validate_values(true, LaunchMode::Hosted, profile, key, lifecycle).is_err());
        }
    }

    #[test]
    fn legacy_takeover_is_explicit_and_never_managed() {
        assert!(validate_values(false, LaunchMode::LegacyTakeover, None, None, None).is_ok());
        assert!(validate_values(true, LaunchMode::LegacyTakeover, None, None, None).is_err());
        assert!(!MANAGED_VARS.contains(&"RIDDLE_SYSTEMD_MANAGED"));
    }

    #[test]
    fn persistent_paths_honor_direct_and_root_overrides() {
        assert_eq!(
            choose_persistent_path(
                Some(PathBuf::from("/test/direct")),
                Some(PathBuf::from("/test/root")),
                true,
                "tasks",
                "/home/root/riddle-data/tasks",
            ),
            PathBuf::from("/test/direct")
        );
        assert_eq!(
            choose_persistent_path(
                None,
                Some(PathBuf::from("/test/root")),
                true,
                "tasks",
                "/home/root/riddle-data/tasks",
            ),
            PathBuf::from("/test/root/tasks")
        );
        assert!(
            !choose_persistent_path(None, None, true, "tasks", "/home/root/riddle-data/tasks",)
                .starts_with("/home/root")
        );
    }
}
