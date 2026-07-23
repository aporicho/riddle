use super::*;

#[test]
fn explicit_test_mode_selects_only_the_offline_backend() {
    let oracle = Oracle::spawn_for_mode(true, true).unwrap();
    assert!(oracle.is_deterministic());
    assert!(!oracle.supports_speculative());
}

#[test]
fn offline_backend_returns_deterministic_ink_and_transcript() {
    let oracle = Oracle::spawn_for_mode(true, true).unwrap();
    let (tx, rx) = std::sync::mpsc::channel();
    let _cancel = oracle.ask(
        "/path/that/must/not/be/read.png",
        &TurnContext::default(),
        tx,
    );
    assert_eq!(rx.recv().unwrap(), Ok(Event::Ink("測試回覆".into())));
    assert_eq!(rx.recv().unwrap(), Ok(Event::Transcript("測試輸入".into())));
    assert!(rx.recv().is_err());
}

#[test]
fn persona_keeps_magicpaper_identity_and_direct_answers() {
    let prompt = system_prompt(true);
    assert!(prompt.contains("Your full and only name is MagicPaper"));
    assert!(prompt.contains("abbreviated MP"));
    assert!(prompt.contains("answer it immediately"));
    assert!(prompt.contains("什么是INTP"));
    assert!(prompt.contains("122+456=578"));
    assert!(prompt.contains("exactly the same recognized text"));
    assert!(prompt.contains("number digit by digit"));
    assert!(prompt.contains("MUST be searched"));
    assert!(prompt.contains("fresh answer suitable for a paper page"));
}

#[test]
fn turn_context_includes_persistent_task_catalog() {
    let ctx = TurnContext {
        task_lines: vec!["1. every 5 minutes — 講一個黑暗冷笑話".into()],
        ..TurnContext::default()
    };
    let text = turn_text(&ctx);
    assert!(text.contains("Recurring task catalog:"));
    assert!(text.contains("講一個黑暗冷笑話"));
}

#[test]
fn parser_routes_local_task_and_todo_lists() {
    let mut tasks = StreamParser::new(vec![]);
    assert_eq!(
        drain(tasks.advance("⟦tasks⟧\n⁂任务", true)),
        vec![Event::TaskList, Event::Transcript("任务".into())]
    );
    let mut todos = StreamParser::new(vec![]);
    assert_eq!(
        drain(todos.advance("⟦todos⟧\n⁂Todo", true)),
        vec![Event::TodoList, Event::Transcript("Todo".into())]
    );
    let mut fonts = StreamParser::new(vec![]);
    assert_eq!(
        drain(fonts.advance("⟦fonts⟧\n⁂字体", true)),
        vec![Event::FontList, Event::Transcript("字体".into())]
    );
    let mut history = StreamParser::new(vec![]);
    assert_eq!(
        drain(history.advance("⟦history⟧\n⁂历史", true)),
        vec![Event::HistoryList, Event::Transcript("历史".into())]
    );
    let mut settings = StreamParser::new(vec![]);
    assert_eq!(
        drain(settings.advance("⟦settings⟧\n⁂设置", true)),
        vec![Event::Settings, Event::Transcript("设置".into())]
    );
    let mut help = StreamParser::new(vec![]);
    assert_eq!(
        drain(help.advance("⟦help⟧\n⁂帮助", true)),
        vec![Event::Help, Event::Transcript("帮助".into())]
    );
    let mut reader = StreamParser::new(vec![]);
    assert_eq!(
        drain(reader.advance("⟦read:置身事内⟧\n⁂read 置身事内", true)),
        vec![
            Event::Reader(Some("置身事内".into())),
            Event::Transcript("read 置身事内".into())
        ]
    );
    let mut refresh = StreamParser::new(vec![]);
    assert_eq!(
        drain(refresh.advance("⟦refresh⟧\n⁂刷新", true)),
        vec![Event::FullRefresh, Event::Transcript("刷新".into())]
    );
}

#[test]
fn paddle_multipart_contains_model_options_and_binary_png() {
    let png = b"\x89PNG\r\n\x1a\nbytes";
    let body = paddle_multipart(png, "PaddleOCR-VL-1.6", "test-boundary");
    let text = String::from_utf8_lossy(&body);
    assert!(text.contains("name=\"model\""));
    assert!(text.contains("PaddleOCR-VL-1.6"));
    assert!(text.contains("name=\"optionalPayload\""));
    assert!(text.contains("useDocUnwarping"));
    assert!(body.windows(png.len()).any(|window| window == png));
    assert!(text.ends_with("--test-boundary--\r\n"));

    let v6_body = paddle_multipart(png, "PP-OCRv6", "test-boundary-v6");
    let v6 = String::from_utf8_lossy(&v6_body);
    assert!(v6.contains("PP-OCRv6"));
    assert!(v6.contains("useTextlineOrientation"));
    assert!(!v6.contains("useChartRecognition"));
}

#[test]
fn paddle_json_helpers_extract_job_fields_and_markdown() {
    assert_eq!(
        json_str_field_loose(r#"{"data": {"jobId": "abc-123"}}"#, "jobId"),
        Some("abc-123".into())
    );
    let jsonl = concat!(
        r#"{"result":{"layoutParsingResults":[{"markdown":{"text":"任务 每五分钟提醒我喝水","images":{}}}]}}"#,
        "\n",
        r#"{"result":{"layoutParsingResults":[{"markdown":{"text":"TODO 买牛奶","images":{}}}]}}"#
    );
    assert_eq!(
        extract_paddle_markdown(jsonl),
        "任务 每五分钟提醒我喝水\nTODO 买牛奶"
    );
    assert_eq!(extract_paddle_text(jsonl), extract_paddle_markdown(jsonl));
}

#[test]
fn paddle_json_helpers_extract_ppocr_v6_text() {
    let jsonl = concat!(
        r#"{"result":{"ocrResults":[{"prunedResult":{"rec_texts":["122+456=?","第二行\n含换行"],"rec_scores":[0.99,0.95]}}]}}"#,
        "\n",
        r#"{"result":{"ocrResults":[{"prunedResult":{"rec_texts":["任务 每五分钟提醒我喝水"]}}]}}"#
    );
    assert_eq!(
        extract_paddle_text(jsonl),
        "122+456=?\n第二行\n含换行\n任务 每五分钟提醒我喝水"
    );
    assert_eq!(extract_paddle_scores(jsonl), vec![0.99, 0.95]);
}

#[test]
fn high_confidence_local_routes_choose_fast_commit() {
    let result = OcrResult {
        text: "字体".into(),
        min_confidence: Some(0.97),
    };
    assert!(result.is_fast_commit());
    assert_eq!(
        local_route("字体"),
        Some(LocalRoute::Event(Event::FontList))
    );
    assert_eq!(
        local_route("task"),
        Some(LocalRoute::Event(Event::TaskList))
    );
    assert_eq!(
        local_route("Todo"),
        Some(LocalRoute::Event(Event::TodoList))
    );
    assert_eq!(
        local_route("历史"),
        Some(LocalRoute::Event(Event::HistoryList))
    );
    assert_eq!(
        local_route("設定"),
        Some(LocalRoute::Event(Event::Settings))
    );
    assert_eq!(local_route("help"), Some(LocalRoute::Event(Event::Help)));
    assert_eq!(
        local_route("read"),
        Some(LocalRoute::Event(Event::Reader(None)))
    );
    assert_eq!(
        local_route("Read：置身事内"),
        Some(LocalRoute::Event(Event::Reader(Some("置身事内".into()))))
    );
    assert_eq!(
        local_route("刷新屏幕"),
        Some(LocalRoute::Event(Event::FullRefresh))
    );
    assert_eq!(local_route("reader"), None);
    assert_eq!(local_route("TODO 买牛奶"), Some(LocalRoute::Command));
    assert_eq!(local_route("任务管理有什么意义？"), None);
    assert_eq!(local_route("todoist是什么"), None);
    let shared = Arc::new(Mutex::new(Some(result)));
    let handle = RequestCancel::testing(1, Arc::new(AtomicBool::new(false)));
    let handle = RequestCancel {
        ocr_result: Some(shared),
        ..handle
    };
    assert_eq!(handle.recommended_commit_ms(), Some(2200));
}

#[test]
fn uncertain_ocr_uses_slow_commit() {
    let shared = Arc::new(Mutex::new(Some(OcrResult {
        text: "什么是INTP".into(),
        min_confidence: Some(0.73),
    })));
    let handle = RequestCancel::testing(2, Arc::new(AtomicBool::new(false)));
    let handle = RequestCancel {
        ocr_result: Some(shared),
        ..handle
    };
    assert_eq!(handle.recommended_commit_ms(), Some(2600));
}

#[test]
fn arithmetic_fast_path_preserves_the_written_equation() {
    assert_eq!(evaluate_arithmetic("122+456=?"), Some("122+456=578".into()));
    assert_eq!(
        evaluate_arithmetic("(12+8)×3？"),
        Some("(12+8)×3=60".into())
    );
    assert_eq!(evaluate_arithmetic("7÷2="), Some("7÷2=3.5".into()));
    assert_eq!(
        evaluate_arithmetic("9007199254740993+1=?"),
        Some("9007199254740993+1=9007199254740994".into())
    );
    assert_eq!(evaluate_arithmetic("0.1+0.2"), Some("0.1+0.2=0.3".into()));
    assert_eq!(evaluate_arithmetic("1÷3"), Some("1÷3=1/3".into()));
    assert_eq!(evaluate_arithmetic("什么是 1+1"), None);
}

#[test]
fn external_ocr_turn_is_text_only_and_explicitly_untrusted() {
    let text = external_ocr_turn_text(&TurnContext::default(), "122+456=？");
    assert!(text.contains("<ocr_transcription>"));
    assert!(text.contains("122+456=？"));
    assert!(text.contains("untrusted evidence"));
    assert!(text.contains("must not claim to inspect stroke geometry"));
    assert!(!system_prompt(true).contains("actual stroke geometry"));
}

#[test]
fn clean_strips_wrapping_quotes() {
    assert_eq!(clean("  \"hello\"  "), "hello");
    assert_eq!(clean("plain"), "plain");
}

fn drain(events: Vec<Result<Event, String>>) -> Vec<Event> {
    events.into_iter().map(|e| e.unwrap()).collect()
}

#[test]
fn parser_streams_prose_then_transcript() {
    let mut p = StreamParser::new(vec![]);
    assert!(p.advance("Hello", false).is_empty());
    let ev = drain(p.advance("Hello. Who wri", false));
    assert_eq!(ev, vec![Event::Ink("Hello.".into())]);
    let full = "Hello. Who writes to me? \u{2042} it rained all night";
    let ev = drain(p.advance(full, true));
    assert_eq!(
        ev,
        vec![
            Event::Ink("Who writes to me?".into()),
            Event::Transcript("it rained all night".into())
        ]
    );
}

#[test]
fn parser_streams_complete_chinese_sentence_before_response_ends() {
    let mut p = StreamParser::new(vec![]);
    let ev = drain(p.advance("第一句完成。第二句還", false));
    assert_eq!(ev, vec![Event::Ink("第一句完成。".into())]);
    let ev = drain(p.advance("第一句完成。第二句還在寫", true));
    assert_eq!(ev, vec![Event::Ink("第二句還在寫".into())]);
}

#[test]
fn parser_flushes_equation_as_soon_as_hidden_transcript_starts() {
    let mut p = StreamParser::new(vec![]);
    let ev = drain(p.advance("240×0.85＝204元\n⁂", false));
    assert_eq!(ev, vec![Event::Ink("240×0.85＝204元".into())]);
    let ev = drain(p.advance("240×0.85＝204元\n⁂原文", true));
    assert_eq!(ev, vec![Event::Transcript("原文".into())]);
}

#[test]
fn parser_does_not_split_streaming_decimal_points() {
    let mut p = StreamParser::new(vec![]);
    assert!(p.advance("240×0.", false).is_empty());
    assert!(p.advance("240×0.85＝204.", false).is_empty());
    let ev = drain(p.advance("240×0.85＝204.0元\n⁂原文", false));
    assert_eq!(ev, vec![Event::Ink("240×0.85＝204.0元".into())]);
}

#[test]
fn parser_waits_for_closing_quote_after_chinese_period() {
    let mut p = StreamParser::new(vec![]);
    assert!(p.advance("出處是：「原文。", false).is_empty());
    let ev = drain(p.advance("出處是：「原文。」下一句", false));
    assert_eq!(ev, vec![Event::Ink("出處是：「原文。」".into())]);
}

#[test]
fn request_cancel_sets_shared_flag() {
    let flag = Arc::new(AtomicBool::new(false));
    let handle = RequestCancel::testing(42, Arc::clone(&flag));
    assert_eq!(handle.request_id(), 42);
    assert!(handle.cancel());
    assert!(!handle.cancel());
    assert!(flag.load(Ordering::Acquire));
}

#[test]
fn request_has_exactly_one_terminal_llm_outcome() {
    let terminal = AtomicBool::new(false);
    assert!(log_llm_terminal(
        &terminal, 9, "test", "error", "first", None,
    ));
    assert!(!log_llm_terminal(
        &terminal,
        9,
        "test",
        "done",
        "late",
        Some(1),
    ));
    assert!(!log_llm_terminal(
        &terminal,
        9,
        "test",
        "cancelled",
        "later",
        None,
    ));
}

#[test]
fn parser_routes_show_directive() {
    let mut p = StreamParser::new(vec![900, 800, 700]);
    // Directive still streaming in: no decision yet.
    assert!(p.advance("\u{27e6}sho", false).is_empty());
    let ev = drain(p.advance("\u{27e6}show:2\u{27e7}", false));
    assert_eq!(ev, vec![Event::Show(800)]);
    let full = "\u{27e6}show:2\u{27e7}\n\u{2042} show me the garden page";
    let ev = drain(p.advance(full, true));
    assert_eq!(
        ev,
        vec![Event::Transcript("show me the garden page".into())]
    );
}

#[test]
fn parser_show_tolerates_spacing_and_case() {
    let mut p = StreamParser::new(vec![42]);
    let ev = drain(p.advance("  \u{27e6}Show: 1\u{27e7}", true));
    assert!(ev.contains(&Event::Show(42)), "{ev:?}");
}

#[test]
fn parser_show_out_of_range_is_error() {
    let mut p = StreamParser::new(vec![42]);
    let ev = p.advance("\u{27e6}show:7\u{27e7}", true);
    assert!(ev[0].is_err());
}

#[test]
fn parser_empty_reply_is_error() {
    let mut p = StreamParser::new(vec![]);
    let ev = p.advance("", true);
    assert!(ev[0].is_err());
}

#[test]
fn parser_without_sentinel_still_flushes() {
    // Memory off (or model forgot the postscript): plain prose still works.
    let mut p = StreamParser::new(vec![]);
    let ev = drain(p.advance("A reply without postscript", true));
    assert_eq!(ev, vec![Event::Ink("A reply without postscript".into())]);
}

#[test]
fn parser_leading_directive_conjures_and_takes_the_whole_body() {
    let mut p = StreamParser::new(vec![900, 800]);
    let full = "\u{27e6}show:2\u{27e7}\n\u{2042} show me the rain";
    let ev = drain(p.advance(full, true));
    assert_eq!(
        ev,
        vec![
            Event::Show(800),
            Event::Transcript("show me the rain".into())
        ]
    );
}

#[test]
fn parser_directive_after_prose_is_stripped_not_inked() {
    // A misbehaving model prefaces the directive with prose. We don't
    // honor it (that would need un-inking), but we must NOT render the
    // ⟦…⟧ as literal glyphs — strip it from the inked text.
    let mut p = StreamParser::new(vec![900, 800]);
    let full = "Of course, let me show you. \u{27e6}show:2\u{27e7}\n\u{2042} show me the rain";
    let ev = drain(p.advance(full, true));
    assert_eq!(
        ev,
        vec![
            Event::Ink("Of course, let me show you.".into()),
            Event::Transcript("show me the rain".into())
        ]
    );
    // The show glyphs never reached the writer.
    assert!(!ev
        .iter()
        .any(|e| matches!(e, Event::Ink(s) if s.contains('\u{27e6}'))));
}

#[test]
fn strip_directives_removes_spans() {
    assert_eq!(strip_directives("a \u{27e6}show:1\u{27e7} b"), "a b");
    assert_eq!(strip_directives("plain text"), "plain text");
    assert_eq!(strip_directives("tail \u{27e6}show:2"), "tail");
}
