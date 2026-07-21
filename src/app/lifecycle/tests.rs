use super::wire::{decode_command, trim_ascii};
use super::*;
use std::io::{Read as _, Write as _};
use std::os::fd::{AsRawFd as _, FromRawFd as _};
use std::os::unix::net::UnixStream;

const START_JSON: &str = r#"{"protocol":2,"request_id":"start-7","body":{"token":{"app_id":"magicpaper","generation":7,"foreground_epoch":0,"lease_id":null},"command":"start","launch_environment":{"app_id":"magicpaper"}}}"#;

fn frame(json: &str) -> Vec<u8> {
    let mut frame = (json.len() as u32).to_be_bytes().to_vec();
    frame.extend_from_slice(json.as_bytes());
    frame
}

fn json_line(json: &str) -> Vec<u8> {
    let mut line = json.as_bytes().to_vec();
    line.push(b'\n');
    line
}

fn event_packets(writes: &std::sync::Mutex<Vec<Vec<u8>>>) -> Vec<serde_json::Value> {
    writes
        .lock()
        .unwrap()
        .iter()
        .map(|packet| {
            let payload = if packet.first() == Some(&b'{') {
                trim_ascii(packet)
            } else {
                let length = u32::from_be_bytes(packet[..4].try_into().unwrap()) as usize;
                assert_eq!(packet.len(), length + 4);
                &packet[4..]
            };
            serde_json::from_slice(payload).unwrap()
        })
        .collect()
}

#[test]
fn fake_transport_decodes_fragmented_and_batched_commands() {
    let first = frame(r#"{"type":"enter_foreground"}"#);
    let second = frame(r#"{"type":"enter_background"}"#);
    let split = 7;
    let mut tail = first[split..].to_vec();
    tail.extend(second);
    let mut client = LifecycleClient::fake(vec![first[..split].to_vec(), tail]);
    assert_eq!(
        client.poll().unwrap(),
        vec![
            LifecycleCommand::EnterForeground,
            LifecycleCommand::EnterBackground,
        ]
    );
}

#[test]
fn managed_client_rejects_legacy_commands_without_tokens() {
    let mut client = LifecycleClient::fake(vec![frame(r#"{"type":"enter_foreground"}"#)]);
    client.require_v2 = true;
    assert_eq!(
        client.poll().unwrap_err().kind(),
        io::ErrorKind::InvalidData
    );
}

#[test]
fn managed_client_requires_a_foreground_start_token_first() {
    for command in [
        r#"{"protocol":2,"request_id":"fg","body":{"token":{"app_id":"magicpaper","generation":7,"foreground_epoch":1,"lease_id":91},"command":"enter_foreground"}}"#,
        START_JSON,
    ] {
        let mut client = LifecycleClient::fake(vec![frame(command)]);
        client.require_v2 = true;
        assert_eq!(
            client.poll().unwrap_err().kind(),
            io::ErrorKind::InvalidData
        );
    }

    let foreground_start = r#"{"protocol":2,"request_id":"start","body":{"token":{"app_id":"magicpaper","generation":7,"foreground_epoch":1,"lease_id":91},"command":"start","launch_environment":{"app_id":"magicpaper"}}}"#;
    let mut client = LifecycleClient::fake(vec![frame(foreground_start)]);
    client.require_v2 = true;
    assert_eq!(client.poll().unwrap(), vec![LifecycleCommand::Start]);
}

#[test]
fn formal_json_line_command_can_be_fragmented_across_stream_reads() {
    let line = json_line(START_JSON);
    let split = 31;
    let mut client = LifecycleClient::fake(vec![line[..split].to_vec(), line[split..].to_vec()]);
    assert_eq!(client.poll().unwrap(), vec![LifecycleCommand::Start]);
    assert_eq!(client.framing, InboundFraming::JsonLines);
}

#[test]
fn ready_waits_for_both_real_frame_and_formal_token() {
    let (mut client, writes) =
        LifecycleClient::fake_with_output(vec![json_line(START_JSON)], true, None);
    client.report_ready_after_frame(41).unwrap();
    assert!(writes.lock().unwrap().is_empty(), "token has not arrived");

    assert_eq!(client.poll().unwrap(), vec![LifecycleCommand::Start]);
    let packets = event_packets(&writes);
    assert_eq!(packets.len(), 1);
    assert_eq!(packets[0]["protocol"], 2);
    assert_eq!(packets[0]["body"]["event"], "ready");
    assert_eq!(packets[0]["body"]["first_frame_sequence"], 41);
    assert_eq!(packets[0]["body"]["token"]["app_id"], "magicpaper");
    assert_eq!(packets[0]["body"]["token"]["generation"], 7);
    assert_eq!(packets[0]["body"]["token"]["foreground_epoch"], 0);
    assert!(packets[0]["body"]["token"]["lease_id"].is_null());
}

#[test]
fn canonical_length_prefixed_commands_produce_length_prefixed_events() {
    let (mut client, writes) =
        LifecycleClient::fake_with_output(vec![frame(START_JSON)], true, None);
    client.report_ready_after_frame(5).unwrap();
    assert_eq!(client.poll().unwrap(), vec![LifecycleCommand::Start]);
    assert_eq!(client.framing, InboundFraming::LengthPrefixed);
    let packets = writes.lock().unwrap();
    assert_eq!(packets.len(), 1);
    let packet = &packets[0];
    let length = u32::from_be_bytes(packet[..4].try_into().unwrap()) as usize;
    assert_eq!(length, packet.len() - 4);
    let event: serde_json::Value = serde_json::from_slice(&packet[4..]).unwrap();
    assert_eq!(event["body"]["event"], "ready");
    assert_eq!(event["body"]["first_frame_sequence"], 5);
}

#[test]
fn ready_echoes_the_token_whose_foreground_frame_was_committed() {
    let foreground = r#"{"protocol":2,"request_id":"fg-1","body":{"token":{"app_id":"magicpaper","generation":7,"foreground_epoch":1,"lease_id":91},"command":"enter_foreground"}}"#;
    let mut commands = frame(START_JSON);
    commands.extend(frame(foreground));
    let (mut client, writes) = LifecycleClient::fake_with_output(vec![commands], true, None);

    client.report_ready_after_frame(12).unwrap();
    assert_eq!(
        client.poll().unwrap(),
        vec![LifecycleCommand::Start, LifecycleCommand::EnterForeground,]
    );
    client.report_ready_after_frame(13).unwrap();

    let packets = event_packets(&writes);
    assert_eq!(packets.len(), 2);
    assert_eq!(packets[0]["body"]["first_frame_sequence"], 12);
    assert_eq!(packets[0]["body"]["token"]["foreground_epoch"], 0);
    assert!(packets[0]["body"]["token"]["lease_id"].is_null());
    assert_eq!(packets[1]["body"]["first_frame_sequence"], 13);
    assert_eq!(packets[1]["body"]["token"]["foreground_epoch"], 1);
    assert_eq!(packets[1]["body"]["token"]["lease_id"], 91);
}

#[test]
fn background_may_revoke_lease_and_events_echo_revoked_token() {
    let foreground = r#"{"protocol":2,"request_id":"fg-1","body":{"token":{"app_id":"magicpaper","generation":7,"foreground_epoch":1,"lease_id":91},"command":"enter_foreground"}}"#;
    let background = r#"{"protocol":2,"request_id":"bg-1","body":{"token":{"app_id":"magicpaper","generation":7,"foreground_epoch":1,"lease_id":null},"command":"enter_background"}}"#;
    let mut commands = frame(START_JSON);
    commands.extend(frame(foreground));
    commands.extend(frame(background));
    let (mut client, writes) = LifecycleClient::fake_with_output(vec![commands], true, None);
    assert_eq!(
        client.poll().unwrap(),
        vec![
            LifecycleCommand::Start,
            LifecycleCommand::EnterForeground,
            LifecycleCommand::EnterBackground,
        ]
    );
    client.report_background_ready().unwrap();
    let packets = event_packets(&writes);
    assert_eq!(packets.len(), 2);
    assert!(packets
        .iter()
        .all(|packet| packet["body"]["token"]["lease_id"].is_null()));
    assert!(packets
        .iter()
        .all(|packet| packet["body"]["token"]["foreground_epoch"] == 1));
}

#[test]
fn one_unix_stream_is_bidirectional_for_commands_and_events() {
    let (client_stream, mut manager_stream) = UnixStream::pair().unwrap();
    client_stream.set_nonblocking(true).unwrap();
    manager_stream
        .set_read_timeout(Some(Duration::from_millis(200)))
        .unwrap();
    let mut client = LifecycleClient::connected(Box::new(client_stream));
    let line = json_line(START_JSON);
    let split = line.len() / 2;

    manager_stream.write_all(&line[..split]).unwrap();
    assert!(client.poll().unwrap().is_empty());
    manager_stream.write_all(&line[split..]).unwrap();
    assert_eq!(client.poll().unwrap(), vec![LifecycleCommand::Start]);

    client.report_ready_after_frame(9).unwrap();
    let mut packet = [0_u8; 2048];
    let received = manager_stream.read(&mut packet).unwrap();
    let event: serde_json::Value = serde_json::from_slice(trim_ascii(&packet[..received])).unwrap();
    assert_eq!(event["body"]["event"], "ready");
    assert_eq!(event["body"]["first_frame_sequence"], 9);
}

#[test]
fn inherited_seqpacket_is_bidirectional_with_canonical_framing() {
    let mut descriptors = [-1; 2];
    let result = unsafe {
        libc::socketpair(
            libc::AF_UNIX,
            libc::SOCK_SEQPACKET | libc::SOCK_NONBLOCK | libc::SOCK_CLOEXEC,
            0,
            descriptors.as_mut_ptr(),
        )
    };
    assert_eq!(result, 0);
    let app_file = unsafe { std::fs::File::from_raw_fd(descriptors[0]) };
    let manager_file = unsafe { std::fs::File::from_raw_fd(descriptors[1]) };
    let mut client = LifecycleClient::connected(Box::new(FdTransport {
        file: app_file,
        message_oriented: true,
    }));
    client.report_ready_after_frame(77).unwrap();

    let command = frame(START_JSON);
    assert_eq!(
        socket_send(manager_file.as_raw_fd(), &command).unwrap(),
        command.len()
    );
    assert_eq!(client.poll().unwrap(), vec![LifecycleCommand::Start]);

    let mut packet = vec![0_u8; MAX_FRAME + 4];
    let received = unsafe {
        libc::recv(
            manager_file.as_raw_fd(),
            packet.as_mut_ptr().cast(),
            packet.len(),
            0,
        )
    };
    assert!(received > 4, "{}", io::Error::last_os_error());
    packet.truncate(received as usize);
    let length = u32::from_be_bytes(packet[..4].try_into().unwrap()) as usize;
    assert_eq!(packet.len(), length + 4);
    let event: serde_json::Value = serde_json::from_slice(&packet[4..]).unwrap();
    assert_eq!(event["body"]["event"], "ready");
    assert_eq!(event["body"]["first_frame_sequence"], 77);
    assert_eq!(event["body"]["token"]["generation"], 7);
}

#[test]
fn background_and_shutdown_events_have_strict_persistence_order() {
    let (mut client, writes) =
        LifecycleClient::fake_with_output(vec![frame(START_JSON)], true, None);
    assert_eq!(client.poll().unwrap(), vec![LifecycleCommand::Start]);

    client.report_background_ready().unwrap();
    let packets = event_packets(&writes);
    assert_eq!(packets.len(), 2);
    assert_eq!(packets[0]["body"]["event"], "state_saved");
    assert_eq!(packets[1]["body"]["event"], "background_ready");
    assert_eq!(packets[1]["body"]["title"], "MagicPaper");
    assert_ne!(packets[0]["request_id"], packets[1]["request_id"]);

    writes.lock().unwrap().clear();
    client
        .report_shutdown_complete(0, Duration::from_millis(20))
        .unwrap();
    let packets = event_packets(&writes);
    assert_eq!(packets.len(), 2);
    assert_eq!(packets[0]["body"]["event"], "state_saved");
    assert_eq!(packets[1]["body"]["event"], "shutdown_complete");
    assert_eq!(packets[1]["body"]["exit_code"], 0);
}

#[test]
fn failed_event_preserves_stage_retryability_and_current_token() {
    let (mut client, writes) =
        LifecycleClient::fake_with_output(vec![frame(START_JSON)], true, None);
    client.poll().unwrap();
    client
        .report_failed(
            LifecycleStage::Runtime,
            "display disconnected",
            true,
            Duration::from_millis(20),
        )
        .unwrap();
    let packets = event_packets(&writes);
    assert_eq!(packets.len(), 1);
    assert_eq!(packets[0]["body"]["event"], "failed");
    assert_eq!(packets[0]["body"]["stage"], "runtime");
    assert_eq!(packets[0]["body"]["message"], "display disconnected");
    assert_eq!(packets[0]["body"]["retryable"], true);
    assert_eq!(packets[0]["body"]["token"]["generation"], 7);
}

#[test]
fn disconnect_without_shutdown_is_a_protocol_failure() {
    let command = frame(r#"{"type":"enter_background"}"#);
    // An empty fake chunk models EOF immediately after the final bytes.
    let mut client = LifecycleClient::fake(vec![command, Vec::new()]);
    let error = client.poll().unwrap_err();
    assert_eq!(error.kind(), io::ErrorKind::ConnectionReset);
}

#[test]
fn explicit_shutdown_remains_valid_when_the_manager_then_disconnects() {
    let command = frame(r#"{"type":"shutdown"}"#);
    let mut client = LifecycleClient::fake(vec![command, Vec::new()]);
    assert_eq!(
        client.poll().unwrap(),
        vec![LifecycleCommand::Shutdown { deadline_ms: 100 }]
    );
}

#[test]
fn manager_aliases_map_to_the_three_application_events() {
    for (kind, expected) in [
        ("resume", LifecycleCommand::EnterForeground),
        ("prepare_park", LifecycleCommand::EnterBackground),
        ("shutdown", LifecycleCommand::Shutdown { deadline_ms: 100 }),
    ] {
        let payload = format!(r#"{{"type":"{kind}"}}"#);
        assert_eq!(
            decode_command(payload.as_bytes()).unwrap().command,
            expected
        );
    }
}

#[test]
fn v2_envelope_is_decoded_and_stale_token_is_ignored() {
    let current = frame(
        r#"{"protocol":2,"request_id":"fg-2","body":{"token":{"app_id":"magicpaper","generation":7,"foreground_epoch":2,"lease_id":11},"command":"enter_foreground"}}"#,
    );
    let stale = frame(
        r#"{"protocol":2,"request_id":"bg-1","body":{"token":{"app_id":"magicpaper","generation":7,"foreground_epoch":1,"lease_id":10},"command":"enter_background"}}"#,
    );
    let mut both = current;
    both.extend(stale);
    let mut client = LifecycleClient::fake(vec![both]);
    assert_eq!(
        client.poll().unwrap(),
        vec![LifecycleCommand::EnterForeground]
    );
}

#[test]
fn v2_envelope_rejects_wrong_app_token() {
    let command = frame(
        r#"{"protocol":2,"request_id":"fg","body":{"token":{"app_id":"koreader","generation":1,"foreground_epoch":1,"lease_id":2},"command":"enter_foreground"}}"#,
    );
    let mut client = LifecycleClient::fake(vec![command]);
    assert_eq!(
        client.poll().unwrap_err().kind(),
        io::ErrorKind::InvalidData
    );
}

#[test]
fn v2_command_payload_fields_do_not_leak_into_runtime_policy() {
    let shutdown = br#"{"protocol":2,"request_id":"stop","body":{"token":{"app_id":"magicpaper","generation":3,"foreground_epoch":4,"lease_id":5},"command":"shutdown","reason":"upgrade","deadline_ms":3500}}"#;
    assert_eq!(
        decode_command(shutdown).unwrap().command,
        LifecycleCommand::Shutdown { deadline_ms: 3500 }
    );
    let open = br#"{"protocol":2,"request_id":"open","body":{"token":{"app_id":"magicpaper","generation":3,"foreground_epoch":5,"lease_id":6},"command":"open_path","path":"/books/a.epub"}}"#;
    assert_eq!(
        decode_command(open).unwrap().command,
        LifecycleCommand::EnterForeground
    );
}

#[test]
fn partial_frame_waits_for_more_data() {
    let complete = frame(r#"{"type":"shutdown"}"#);
    let mut bytes = complete[..6].to_vec();
    let mut framing = InboundFraming::Unknown;
    assert!(decode_frames(&mut bytes, &mut framing, false)
        .unwrap()
        .is_empty());
    assert_eq!(bytes, complete[..6]);
}

#[test]
fn oversized_frame_is_rejected() {
    let mut bytes = ((MAX_FRAME + 1) as u32).to_be_bytes().to_vec();
    let mut framing = InboundFraming::Unknown;
    assert_eq!(
        decode_frames(&mut bytes, &mut framing, false)
            .unwrap_err()
            .kind(),
        io::ErrorKind::InvalidData
    );
}
