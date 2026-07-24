use super::*;

fn temp_dir(name: &str) -> PathBuf {
    let path = std::env::temp_dir().join(format!(
        "magicpaper-pi-preferences-{}-{name}",
        std::process::id()
    ));
    let _ = std::fs::remove_dir_all(&path);
    path
}

#[test]
fn missing_corrupt_and_future_settings_use_agent_defaults() {
    let dir = temp_dir("defaults");
    let preferences = PiPreferences::open_in(&dir);
    assert_eq!(preferences.values(), PiPreferenceValues::default());
    assert_eq!(preferences.values().provider, PiProvider::OpenAi);
    assert_eq!(
        preferences
            .values()
            .model
            .model_id(preferences.values().provider),
        "gpt-5.6-terra"
    );
    assert_eq!(preferences.values().thinking, PiThinking::Off);
    assert!(preferences.values().tools_enabled);

    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(dir.join(PI_SETTINGS_FILE), b"not-json").unwrap();
    assert_eq!(
        PiPreferences::open_in(&dir).values(),
        PiPreferenceValues::default()
    );
    std::fs::write(dir.join(PI_SETTINGS_FILE), br#"{"schema":99,"values":{}}"#).unwrap();
    assert_eq!(
        PiPreferences::open_in(&dir).values(),
        PiPreferenceValues::default()
    );
    let _ = std::fs::remove_dir_all(dir);
}

#[test]
fn oversized_settings_use_defaults_without_unbounded_loading() {
    let dir = temp_dir("oversized");
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(
        dir.join(PI_SETTINGS_FILE),
        vec![b'x'; MAX_PI_SETTINGS_BYTES + 1],
    )
    .unwrap();
    assert_eq!(
        PiPreferences::open_in(&dir).values(),
        PiPreferenceValues::default()
    );
    let _ = std::fs::remove_dir_all(dir);
}

#[test]
fn settings_round_trip_through_versioned_atomic_file() {
    let dir = temp_dir("round-trip");
    let mut preferences = PiPreferences::open_in(&dir);
    let values = PiPreferenceValues {
        provider: PiProvider::OpenAi,
        model: PiModel::DeepSeekV4Pro,
        thinking: PiThinking::Maximum,
        tools_enabled: false,
    };
    preferences.replace(values).unwrap();

    assert_eq!(PiPreferences::open_in(&dir).values(), values);
    assert!(!dir.join("pi-settings.json.new").exists());
    let body = std::fs::read_to_string(dir.join(PI_SETTINGS_FILE)).unwrap();
    assert!(body.contains("\"schema\": 1"));
    assert!(body.contains("\"deepseek-v4-pro\""));
    let _ = std::fs::remove_dir_all(dir);
}

#[test]
fn old_openai_codex_label_migrates_to_the_direct_openai_provider() {
    let dir = temp_dir("openai-provider-migration");
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(
        dir.join(PI_SETTINGS_FILE),
        br#"{"schema":1,"values":{"provider":"open_ai_codex","model":"deepseek-v4-flash","thinking":"off","tools_enabled":true}}"#,
    )
    .unwrap();
    let values = PiPreferences::open_in(&dir).values();
    assert_eq!(values.provider, PiProvider::OpenAi);
    assert_eq!(values.provider.provider_id(), "openai");
    assert_eq!(values.model.model_id(values.provider), "gpt-5.6-terra");
    let _ = std::fs::remove_dir_all(dir);
}

#[test]
fn thinking_selector_uses_only_levels_supported_by_packaged_models() {
    assert_eq!(PiThinking::Off.next(PiProvider::DeepSeek), PiThinking::High);
    assert_eq!(
        PiThinking::High.next(PiProvider::DeepSeek),
        PiThinking::Maximum
    );
    assert_eq!(
        PiThinking::Maximum.next(PiProvider::DeepSeek),
        PiThinking::Off
    );
    assert_eq!(
        PiThinking::Off.next(PiProvider::OpenAi),
        PiThinking::ExtraHigh
    );
    assert_eq!(
        PiThinking::ExtraHigh.next(PiProvider::OpenAi),
        PiThinking::Maximum
    );
    assert_eq!(
        PiThinking::Low.normalized(PiProvider::DeepSeek),
        PiThinking::Off
    );
    assert_eq!(
        PiThinking::High.normalized(PiProvider::OpenAi),
        PiThinking::Off
    );
}

#[cfg(unix)]
#[test]
fn settings_replacement_never_follows_a_stale_temporary_symlink() {
    use std::os::unix::fs::symlink;

    let dir = temp_dir("temporary-symlink");
    std::fs::create_dir_all(&dir).unwrap();
    let victim = dir.join("victim");
    std::fs::write(&victim, b"keep-me").unwrap();
    symlink(&victim, dir.join("pi-settings.json.new")).unwrap();
    let mut preferences = PiPreferences::open_in(&dir);
    preferences.replace(PiPreferenceValues::default()).unwrap();
    assert_eq!(std::fs::read(victim).unwrap(), b"keep-me");
    let _ = std::fs::remove_dir_all(dir);
}
