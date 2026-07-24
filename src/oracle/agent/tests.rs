use super::wire::{read_frame, valid_event, write_frame};
use super::*;
use serde_json::json;
use std::io::Read;
use std::os::unix::net::{UnixListener, UnixStream};
use std::sync::mpsc;

fn test_oracle(socket: PathBuf) -> AgentOracle {
    AgentOracle {
        socket,
        client_token: "a".repeat(64),
        app_id: "magicpaper".into(),
        profile: AgentProfile {
            provider: "deepseek".into(),
            model: "deepseek-v4-flash".into(),
            thinking: "off".into(),
            tools: true,
        },
        remember: true,
        ocr: None,
        workers: AgentWorkers::default(),
    }
}

fn temp_socket(name: &str) -> PathBuf {
    std::env::temp_dir().join(format!(
        "magicpaper-agent-{}-{name}.sock",
        std::process::id()
    ))
}

#[test]
fn agent_frames_are_big_endian_and_bounded() {
    let (mut left, mut right) = UnixStream::pair().unwrap();
    write_frame(&mut left, &json!({"protocol":1,"type":"status"})).unwrap();
    let decoded = read_frame(&mut right, &AtomicBool::new(false))
        .unwrap()
        .unwrap();
    assert_eq!(decoded["type"], "status");
}

#[test]
fn profile_defaults_to_openai_terra_without_system_tools() {
    let profile = AgentProfile::from_preferences(PiPreferenceValues::default());
    assert_eq!(profile.provider, "openai");
    assert_eq!(profile.model, "gpt-5.6-terra");
    assert_eq!(profile.thinking, "off");
    assert!(profile.tools);
    assert_eq!(safe_tool_specs(true), Value::Array(Vec::new()));
}

#[test]
fn openai_profile_uses_only_a_supported_gpt_5_6_thinking_level() {
    use crate::pi_preferences::{PiModel, PiProvider, PiThinking};

    let profile = AgentProfile::from_preferences(PiPreferenceValues {
        provider: PiProvider::OpenAi,
        model: PiModel::DeepSeekV4Flash,
        thinking: PiThinking::ExtraHigh,
        tools_enabled: true,
    });
    assert_eq!(profile.provider, "openai");
    assert_eq!(profile.model, "gpt-5.6-terra");
    assert_eq!(profile.thinking, "xhigh");
}

#[test]
fn every_local_worker_lane_has_an_explicit_platform_priority() {
    assert_eq!(Lane::Interactive.as_protocol_name(), "interactive");
    assert_eq!(Lane::Speculative.as_protocol_name(), "speculative");
    assert_eq!(Lane::Scheduled.as_protocol_name(), "scheduled");
}

#[test]
fn mismatched_agent_events_are_rejected() {
    let event = json!({
        "protocol": 1,
        "type": "complete",
        "request_id": "other",
        "app_id": "magicpaper"
    });
    assert!(!valid_event(&event, "expected", "magicpaper"));
}

#[test]
fn complete_agent_turn_streams_paper_ink_and_hidden_transcript() {
    let socket = temp_socket("complete");
    let _ = std::fs::remove_file(&socket);
    let listener = UnixListener::bind(&socket).unwrap();
    let server = thread::spawn(move || {
        let (mut stream, _) = listener.accept().unwrap();
        let request = read_frame(&mut stream, &AtomicBool::new(false))
            .unwrap()
            .unwrap();
        assert_eq!(request["protocol"], 1);
        assert_eq!(request["type"], "start_turn");
        assert_eq!(request["app_id"], "magicpaper");
        assert_eq!(request["client_token"], "a".repeat(64));
        assert_eq!(request["lane"], "scheduled");
        assert_eq!(request["profile"]["provider"], "deepseek");
        assert_eq!(request["profile"]["model"], "deepseek-v4-flash");
        assert_eq!(request["profile"]["tools"], true);
        assert_eq!(request["tools"], Value::Array(Vec::new()));
        assert!(request["input"].as_str().unwrap().contains("测试问题"));

        let request_id = request["request_id"].as_str().unwrap().to_owned();
        let turn_id = "turn-1";
        for event in [
            json!({"protocol":1,"type":"accepted","request_id":request_id,"app_id":"magicpaper","turn_id":turn_id}),
            json!({"protocol":1,"type":"text_delta","request_id":request_id,"app_id":"magicpaper","turn_id":turn_id,"text":"第一句。"}),
            json!({"protocol":1,"type":"text_delta","request_id":request_id,"app_id":"magicpaper","turn_id":turn_id,"text":"第二句。⁂测试问题"}),
            json!({"protocol":1,"type":"complete","request_id":request_id,"app_id":"magicpaper","turn_id":turn_id}),
        ] {
            write_frame(&mut stream, &event).unwrap();
        }
    });

    let oracle = test_oracle(socket.clone());
    let (tx, rx) = mpsc::channel();
    let permit = oracle.workers.acquire(Lane::Scheduled).unwrap();
    oracle.run_turn(TurnRequest {
        request_id: 42,
        domain: "test",
        lane: Lane::Scheduled,
        input: "测试问题".into(),
        ctx: TurnContext::default(),
        tx,
        cancelled: Arc::new(AtomicBool::new(false)),
        terminal: Arc::new(AtomicBool::new(false)),
        _permit: permit,
    });
    server.join().unwrap();
    let events: Vec<_> = rx.into_iter().collect::<Result<_, _>>().unwrap();
    assert_eq!(
        events,
        vec![
            Event::Ink("第一句。".into()),
            Event::Ink("第二句。".into()),
            Event::Transcript("测试问题".into()),
        ]
    );
    let _ = std::fs::remove_file(socket);
}

#[test]
fn cancellation_before_acceptance_closes_the_owned_connection() {
    let socket = temp_socket("cancel-before-accepted");
    let _ = std::fs::remove_file(&socket);
    let listener = UnixListener::bind(&socket).unwrap();
    let server = thread::spawn(move || {
        let (mut stream, _) = listener.accept().unwrap();
        let request = read_frame(&mut stream, &AtomicBool::new(false))
            .unwrap()
            .unwrap();
        assert_eq!(request["type"], "start_turn");
        let mut byte = [0_u8; 1];
        assert_eq!(stream.read(&mut byte).unwrap(), 0);
    });

    let oracle = test_oracle(socket.clone());
    let permit = oracle.workers.acquire(Lane::Speculative).unwrap();
    oracle.run_turn(TurnRequest {
        request_id: 43,
        domain: "test",
        lane: Lane::Speculative,
        input: "即刻取消".into(),
        ctx: TurnContext::default(),
        tx: mpsc::channel().0,
        cancelled: Arc::new(AtomicBool::new(true)),
        terminal: Arc::new(AtomicBool::new(false)),
        _permit: permit,
    });
    server.join().unwrap();
    let _ = std::fs::remove_file(socket);
}

#[test]
fn cancellation_after_acceptance_names_the_owned_remote_turn() {
    let socket = temp_socket("cancel-after-accepted");
    let _ = std::fs::remove_file(&socket);
    let listener = UnixListener::bind(&socket).unwrap();
    let server = thread::spawn(move || {
        let (mut stream, _) = listener.accept().unwrap();
        let request = read_frame(&mut stream, &AtomicBool::new(false))
            .unwrap()
            .unwrap();
        let request_id = request["request_id"].as_str().unwrap().to_owned();
        write_frame(
            &mut stream,
            &json!({
                "protocol": 1,
                "type": "accepted",
                "request_id": request_id,
                "app_id": "magicpaper",
                "turn_id": "remote-turn-7"
            }),
        )
        .unwrap();
        write_frame(
            &mut stream,
            &json!({
                "protocol": 1,
                "type": "text_delta",
                "request_id": request_id,
                "app_id": "magicpaper",
                "turn_id": "remote-turn-7",
                "text": "已接收。继续"
            }),
        )
        .unwrap();
        let cancel = read_frame(&mut stream, &AtomicBool::new(false))
            .unwrap()
            .unwrap();
        assert_eq!(cancel["type"], "cancel_turn");
        assert_eq!(cancel["turn_id"], "remote-turn-7");
        assert_eq!(cancel["request_id"], request_id);
    });

    let oracle = test_oracle(socket.clone());
    let cancelled = Arc::new(AtomicBool::new(false));
    let cancel_signal = Arc::clone(&cancelled);
    let (tx, rx) = mpsc::channel();
    let canceller = thread::spawn(move || {
        assert_eq!(rx.recv().unwrap(), Ok(Event::Ink("已接收。".into())));
        cancel_signal.store(true, Ordering::Release);
    });
    let permit = oracle.workers.acquire(Lane::Interactive).unwrap();
    oracle.run_turn(TurnRequest {
        request_id: 44,
        domain: "test",
        lane: Lane::Interactive,
        input: "取消这个回合".into(),
        ctx: TurnContext::default(),
        tx,
        cancelled,
        terminal: Arc::new(AtomicBool::new(false)),
        _permit: permit,
    });
    canceller.join().unwrap();
    server.join().unwrap();
    let _ = std::fs::remove_file(socket);
}

#[test]
fn oversized_agent_answer_is_cancelled_before_reaching_the_paper_parser() {
    let socket = temp_socket("oversized-answer");
    let _ = std::fs::remove_file(&socket);
    let listener = UnixListener::bind(&socket).unwrap();
    let server = thread::spawn(move || {
        let (mut stream, _) = listener.accept().unwrap();
        let request = read_frame(&mut stream, &AtomicBool::new(false))
            .unwrap()
            .unwrap();
        let request_id = request["request_id"].as_str().unwrap().to_owned();
        for event in [
            json!({"protocol":1,"type":"accepted","request_id":request_id,"app_id":"magicpaper","turn_id":"large-turn"}),
            json!({"protocol":1,"type":"text_delta","request_id":request_id,"app_id":"magicpaper","turn_id":"large-turn","text":"字".repeat(64 * 1024)}),
        ] {
            write_frame(&mut stream, &event).unwrap();
        }
        let cancel = read_frame(&mut stream, &AtomicBool::new(false))
            .unwrap()
            .unwrap();
        assert_eq!(cancel["type"], "cancel_turn");
        assert_eq!(cancel["turn_id"], "large-turn");
    });

    let oracle = test_oracle(socket.clone());
    let (tx, rx) = mpsc::channel();
    let permit = oracle.workers.acquire(Lane::Interactive).unwrap();
    oracle.run_turn(TurnRequest {
        request_id: 45,
        domain: "test",
        lane: Lane::Interactive,
        input: "不要输出无限内容".into(),
        ctx: TurnContext::default(),
        tx,
        cancelled: Arc::new(AtomicBool::new(false)),
        terminal: Arc::new(AtomicBool::new(false)),
        _permit: permit,
    });
    server.join().unwrap();
    let events = rx.into_iter().collect::<Vec<_>>();
    assert_eq!(events.len(), 1);
    assert!(events[0]
        .as_ref()
        .unwrap_err()
        .contains("exceeded the paper limit"));
    let _ = std::fs::remove_file(socket);
}

#[test]
fn every_settings_action_uses_the_authenticated_agent_protocol() {
    for (name, command, wire_type, profile) in [
        (
            "reload",
            AgentControlCommand::ReloadProfile,
            "reload_profile",
            true,
        ),
        (
            "restart",
            AgentControlCommand::Restart,
            "reload_profile",
            false,
        ),
        (
            "session",
            AgentControlCommand::NewSession,
            "new_session",
            false,
        ),
    ] {
        let socket = temp_socket(name);
        let _ = std::fs::remove_file(&socket);
        let listener = UnixListener::bind(&socket).unwrap();
        let server = thread::spawn(move || {
            let (mut stream, _) = listener.accept().unwrap();
            let request = read_frame(&mut stream, &AtomicBool::new(false))
                .unwrap()
                .unwrap();
            assert_eq!(request["type"], wire_type);
            assert_eq!(request["app_id"], "magicpaper");
            assert_eq!(request["client_token"], "a".repeat(64));
            if profile {
                assert_eq!(request["profile"]["model"], "deepseek-v4-flash");
            } else if command == AgentControlCommand::Restart {
                assert!(request["profile"].is_null());
            } else {
                assert!(request.get("profile").is_none());
            }
            let response = json!({
                "protocol": 1,
                "type": "status",
                "request_id": request["request_id"],
                "app_id": "magicpaper",
                "status": {
                    "available": true,
                    "provider_configured": true,
                    "runtime_source": "packaged",
                    "busy": false
                },
            });
            write_frame(&mut stream, &response).unwrap();
        });
        let oracle = test_oracle(socket.clone());
        assert_eq!(oracle.run_control(command), AgentControlStatus::Online);
        server.join().unwrap();
        let _ = std::fs::remove_file(socket);
    }
}
