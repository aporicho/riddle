//! Short-lived control requests used by the paper-native Pi settings panel.

use super::wire::{read_frame, valid_event, write_frame};
use super::AgentOracle;
use serde_json::{json, Value};
use std::os::unix::net::UnixStream;
use std::sync::atomic::AtomicBool;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum AgentControlCommand {
    ReloadProfile,
    Restart,
    NewSession,
}

impl AgentControlCommand {
    fn wire_type(self) -> &'static str {
        match self {
            Self::ReloadProfile | Self::Restart => "reload_profile",
            Self::NewSession => "new_session",
        }
    }

    fn label(self) -> &'static str {
        match self {
            Self::ReloadProfile => "reload",
            Self::Restart => "restart",
            Self::NewSession => "session",
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum AgentControlStatus {
    Starting,
    Online,
    MissingKey,
    MissingRuntime,
    StorageError,
    NetworkError,
}

impl AgentOracle {
    pub(super) fn run_control(&self, command: AgentControlCommand) -> AgentControlStatus {
        let mut stream = match UnixStream::connect(&self.socket) {
            Ok(stream) => stream,
            Err(_) => return AgentControlStatus::NetworkError,
        };
        let _ = stream.set_read_timeout(Some(Duration::from_secs(5)));
        let request_id = control_request_id(command);
        let mut request = json!({
            "protocol": 1,
            "type": command.wire_type(),
            "request_id": request_id,
            "app_id": self.app_id,
            "client_token": self.client_token,
        });
        if command == AgentControlCommand::ReloadProfile {
            request["profile"] = json!({
                "provider": self.profile.provider,
                "model": self.profile.model,
                "thinking": self.profile.thinking,
                "tools": self.profile.tools,
            });
        } else if command == AgentControlCommand::Restart {
            request["profile"] = Value::Null;
        }
        if write_frame(&mut stream, &request).is_err() {
            return AgentControlStatus::NetworkError;
        }
        let event = match read_frame(&mut stream, &AtomicBool::new(false)) {
            Ok(Some(event)) if valid_event(&event, &request_id, &self.app_id) => event,
            _ => return AgentControlStatus::NetworkError,
        };
        match event.get("type").and_then(Value::as_str) {
            Some("status") => classify_status(&event),
            Some("error") => classify_error(&event),
            _ => AgentControlStatus::NetworkError,
        }
    }
}

fn control_request_id(command: AgentControlCommand) -> String {
    let micros = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_micros();
    format!("magicpaper-control-{}-{micros}", command.label())
}

fn classify_status(event: &Value) -> AgentControlStatus {
    let status = &event["status"];
    if status["runtime_source"].as_str() == Some("missing")
        || status["available"].as_bool() != Some(true)
    {
        AgentControlStatus::MissingRuntime
    } else if status["provider_configured"].as_bool() != Some(true) {
        AgentControlStatus::MissingKey
    } else {
        AgentControlStatus::Online
    }
}

fn classify_error(event: &Value) -> AgentControlStatus {
    if event.get("code").and_then(Value::as_str) == Some("unavailable") {
        return AgentControlStatus::MissingRuntime;
    }
    let message = event
        .get("message")
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_ascii_lowercase();
    if message.contains("api key") || message.contains("credential") || message.contains("密钥") {
        AgentControlStatus::MissingKey
    } else {
        AgentControlStatus::NetworkError
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn error_status_never_exposes_the_remote_message() {
        assert_eq!(
            classify_error(&json!({"message":"missing API key sk-secret"})),
            AgentControlStatus::MissingKey
        );
        assert_eq!(
            classify_error(&json!({"message":"connection refused"})),
            AgentControlStatus::NetworkError
        );
        assert_eq!(
            classify_error(&json!({"code":"unavailable","message":"details"})),
            AgentControlStatus::MissingRuntime
        );
    }

    #[test]
    fn status_requires_both_the_runtime_and_selected_provider_secret() {
        assert_eq!(
            classify_status(&json!({"status": {
                "available": false,
                "provider_configured": true,
                "runtime_source": "missing"
            }})),
            AgentControlStatus::MissingRuntime
        );
        assert_eq!(
            classify_status(&json!({"status": {
                "available": true,
                "provider_configured": false,
                "runtime_source": "packaged"
            }})),
            AgentControlStatus::MissingKey
        );
        assert_eq!(
            classify_status(&json!({"status": {
                "available": true,
                "provider_configured": true,
                "runtime_source": "packaged"
            }})),
            AgentControlStatus::Online
        );
    }
}
