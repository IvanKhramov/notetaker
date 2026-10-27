use std::{
    io::{BufRead, BufReader, Read, Write},
    net::TcpListener,
    thread::{self, JoinHandle},
    time::{Duration, Instant},
};

use super::*;

struct Mock {
    base: String,
    handle: JoinHandle<(String, Value)>,
}

impl Mock {
    fn start(status: &str, content_type: &str, body: &str, extra_headers: &str) -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        listener.set_nonblocking(true).unwrap();
        let base = format!("http://{}/v1", listener.local_addr().unwrap());
        let response = format!(
            "HTTP/1.1 {status}\r\nContent-Type: {content_type}\r\nContent-Length: {}\r\nConnection: close\r\n{extra_headers}\r\n{body}",
            body.len()
        );
        let handle = thread::spawn(move || {
            let until = Instant::now() + Duration::from_secs(10);
            let mut stream = loop {
                match listener.accept() {
                    Ok((stream, _)) => break stream,
                    Err(error)
                        if error.kind() == std::io::ErrorKind::WouldBlock
                            && Instant::now() < until =>
                    {
                        thread::sleep(Duration::from_millis(5))
                    }
                    Err(error) => panic!("mock server did not receive a request: {error}"),
                }
            };
            // Windows sockets inherit non-blocking mode from the listener.
            stream.set_nonblocking(false).unwrap();
            stream
                .set_read_timeout(Some(Duration::from_secs(5)))
                .unwrap();
            let mut reader = BufReader::new(&stream);
            let mut headers = String::new();
            let mut size = 0;
            loop {
                let mut line = String::new();
                assert!(reader.read_line(&mut line).unwrap() > 0);
                if line == "\r\n" {
                    break;
                }
                if let Some(value) = line.to_lowercase().strip_prefix("content-length:") {
                    size = value.trim().parse().unwrap();
                }
                headers.push_str(&line);
            }
            let mut body = vec![0; size];
            reader.read_exact(&mut body).unwrap();
            stream.write_all(response.as_bytes()).unwrap();
            (
                headers,
                if body.is_empty() {
                    Value::Null
                } else {
                    serde_json::from_slice(&body).unwrap()
                },
            )
        });
        Self { base, handle }
    }

    fn provider(&self, kind: ProviderKind) -> HttpProvider {
        HttpProvider::build(
            ProviderConfig {
                id: "mock".into(),
                kind,
                base_url: self.base.clone(),
                model: "test-model".into(),
                api_key_env: None,
                key_source: Default::default(),
                context_tokens: 4096,
            },
            Mode::Local,
            Some("test-key".into()),
        )
        .unwrap()
    }
}

fn request() -> CompletionRequest {
    CompletionRequest {
        system: "system instruction".into(),
        user: "user notes".into(),
        max_output_tokens: 123,
    }
}

#[test]
fn openai_streams_sse_deltas_and_sends_chat_request() {
    let body = concat!(
        ": heartbeat\r\n\r\n",
        "data: {\"choices\":[{\"delta\":{\"role\":\"assistant\"}}]}\r\n\r\n",
        "data: {\"choices\":[{\"delta\":{\"content\":\"# Итоги\"}}]}\r\n\r\n",
        "data: {\"choices\":[{\"delta\":{\"content\":\"\\nГотово\"},\"finish_reason\":\"stop\"}]}\r\n\r\n",
        "data: [DONE]\r\n\r\n",
    );
    let server = Mock::start("200 OK", "text/event-stream; charset=utf-8", body, "");
    let provider = server.provider(ProviderKind::Openai);
    let mut deltas = Vec::new();
    let text = provider
        .complete(&request(), &mut |part| deltas.push(part.to_owned()))
        .unwrap();
    assert_eq!(deltas, ["# Итоги", "\nГотово"]);
    assert_eq!(text, deltas.concat());
    let (headers, body) = server.handle.join().unwrap();
    assert!(headers.starts_with("POST /v1/chat/completions HTTP/1.1"));
    assert!(
        headers
            .to_lowercase()
            .contains("authorization: bearer test-key")
    );
    assert_eq!(body["model"], "test-model");
    assert_eq!(body["messages"][0]["role"], "system");
    assert_eq!(body["messages"][1]["content"], "user notes");
    assert_eq!(body["max_tokens"], 123);
    assert_eq!(body["stream"], true);
}

#[test]
fn openai_accepts_nonstreaming_compatible_server() {
    let server = Mock::start(
        "200 OK",
        "application/json",
        r##"{"choices":[{"message":{"content":"# Notes"},"finish_reason":"stop"}]}"##,
        "",
    );
    let provider = server.provider(ProviderKind::Openai);
    let mut deltas = String::new();
    assert_eq!(
        provider
            .complete(&request(), &mut |text| deltas.push_str(text))
            .unwrap(),
        "# Notes"
    );
    assert_eq!(deltas, "# Notes");
    server.handle.join().unwrap();
}

#[test]
fn anthropic_uses_messages_headers_and_text_blocks() {
    let server = Mock::start(
        "200 OK",
        "application/json",
        r##"{"content":[{"type":"text","text":"# Notes"},{"type":"text","text":"\nDone"}],"stop_reason":"end_turn"}"##,
        "",
    );
    let provider = server.provider(ProviderKind::Anthropic);
    let mut deltas = String::new();
    assert_eq!(
        provider
            .complete(&request(), &mut |text| deltas.push_str(text))
            .unwrap(),
        "# Notes\nDone"
    );
    assert_eq!(deltas, "# Notes\nDone");
    let (headers, body) = server.handle.join().unwrap();
    let headers = headers.to_lowercase();
    assert!(headers.starts_with("post /v1/messages http/1.1"));
    assert!(headers.contains("x-api-key: test-key"));
    assert!(headers.contains("anthropic-version: 2023-06-01"));
    assert!(!headers.contains("authorization:"));
    assert_eq!(body["system"], "system instruction");
    assert_eq!(body["messages"][0]["role"], "user");
    assert_eq!(body["max_tokens"], 123);
}

#[test]
fn both_providers_report_4xx_and_5xx_without_echoing_response_bodies() {
    for kind in [ProviderKind::Openai, ProviderKind::Anthropic] {
        for status in ["401 Unauthorized", "503 Service Unavailable"] {
            let server = Mock::start(
                status,
                "application/json",
                r#"{"error":"private input and test-key"}"#,
                "",
            );
            let error = server
                .provider(kind)
                .complete(&request(), &mut |_| {})
                .unwrap_err()
                .to_string();
            assert!(error.contains(status));
            assert!(!error.contains("private input"));
            assert!(!error.contains("test-key"));
            server.handle.join().unwrap();
        }
    }
}

#[test]
fn local_only_rejects_remote_before_reading_key_or_connecting() {
    for host in [
        "example.invalid",
        "localhost.example.invalid",
        "192.0.2.1",
        "[2001:db8::1]",
    ] {
        let config = ProviderConfig {
            id: "remote".into(),
            kind: ProviderKind::Openai,
            base_url: format!("http://{host}/v1"),
            model: "model".into(),
            api_key_env: Some("NOTETAKER_MISSING_KEY_LOCAL_POLICY_TEST".into()),
            key_source: Default::default(),
            context_tokens: 4096,
        };
        let error = HttpProvider::new(config, Mode::Local)
            .err()
            .unwrap()
            .to_string();
        assert!(error.contains("local-only"));
    }
}

#[test]
fn local_only_does_not_follow_redirects() {
    let server = Mock::start(
        "307 Temporary Redirect",
        "application/json",
        "{}",
        "Location: https://example.invalid/v1/chat/completions\r\n",
    );
    let error = server
        .provider(ProviderKind::Openai)
        .complete(&request(), &mut |_| {})
        .unwrap_err()
        .to_string();
    assert!(error.contains("307"));
    server.handle.join().unwrap();
}

#[test]
fn incomplete_or_truncated_streams_are_errors() {
    for (body, expected) in [
        (
            "data: {\"choices\":[{\"delta\":{\"content\":\"partial\"}}]}\n\n",
            "before completion",
        ),
        (
            "data: {\"choices\":[{\"delta\":{},\"finish_reason\":\"length\"}]}\n\n",
            "token limit",
        ),
        (
            "data: {\"error\":{\"message\":\"private data\"}}\n\n",
            "provider error",
        ),
    ] {
        let server = Mock::start("200 OK", "text/event-stream", body, "");
        let error = server
            .provider(ProviderKind::Openai)
            .complete(&request(), &mut |_| {})
            .unwrap_err()
            .to_string();
        assert!(error.contains(expected));
        assert!(!error.contains("private data"));
        server.handle.join().unwrap();
    }
}

#[test]
fn parses_models_and_rejects_missing_or_empty_lists() {
    let models = parse_models(
        json!({"data":[{"id":"z"},{"id":"a"},{"id":"a"},{"id":""},{"other":"ignored"}]}),
    )
    .unwrap();
    assert_eq!(models, ["z", "a"]);
    assert!(parse_models(json!({"models":[]})).is_err());
    assert!(parse_models(json!({"data":[]})).is_err());
}

#[test]
fn models_use_api_root_and_authorization_without_completion_request() {
    for kind in [ProviderKind::Openai, ProviderKind::Anthropic] {
        let server = Mock::start(
            "200 OK",
            "application/json",
            r#"{"data":[{"id":"claude-sonnet-4-6"},{"id":"other"}]}"#,
            "",
        );
        let models = server.provider(kind).models().unwrap();
        assert_eq!(models, ["claude-sonnet-4-6", "other"]);
        let (headers, body) = server.handle.join().unwrap();
        assert!(headers.starts_with("GET /v1/models HTTP/1.1"));
        assert_eq!(body, Value::Null);
        let headers = headers.to_lowercase();
        if kind == ProviderKind::Openai {
            assert!(headers.contains("authorization: bearer test-key"));
        } else {
            assert!(headers.contains("x-api-key: test-key"));
        }
    }
}

#[test]
fn chat_streams_with_citations_attendees_and_local_fallback() {
    let server = Mock::start(
        "200 OK",
        "text/event-stream",
        concat!(
            "data: {\"choices\":[{\"delta\":{\"content\":\"Иван: готово [1]\"},\"finish_reason\":\"stop\"}]}\n\n",
            "data: [DONE]\n\n"
        ),
        "",
    );
    let root = tempfile::tempdir().unwrap();
    let library = notetaker_core::Library::open(root.path()).unwrap();
    let now = chrono::DateTime::from_timestamp(1_800_000_000, 0).unwrap();
    let mut meta = notetaker_core::MeetingMeta::new("План", now);
    meta.calendar_event = Some(notetaker_core::calendar::EventLink {
        uid: "event".into(),
        title: "План".into(),
        call_url: None,
        attendees: vec![notetaker_core::calendar::Attendee {
            name: "Иван".into(),
            email: "ivan@example.org".into(),
        }],
    });
    let meeting = library.create_meeting(meta).unwrap();
    meeting.write_notes("Выпуск готов").unwrap();
    let mut transcript = notetaker_core::Transcript::default();
    transcript.rename_speaker("s1", "Иван");
    transcript.segments.push(notetaker_core::Segment {
        id: meeting.read_meta().unwrap().id,
        speaker: notetaker_core::SpeakerId("s1".into()),
        channel: notetaker_core::Channel::System,
        start_ms: 0,
        end_ms: 1000,
        text: "готово".into(),
        source: notetaker_core::Source::Final,
        edited: false,
    });
    meeting.write_transcript(&transcript).unwrap();
    let config = crate::Config::parse(&format!("[defaults]\nlocal='local'\nhybrid='cloud'\n[[providers]]\nid='local'\nkind='openai'\nbase_url='{}'\nmodel='local-chat'\ncontext_tokens=4096\n[[providers]]\nid='cloud'\nkind='openai'\nbase_url='https://example.invalid/v1'\nmodel='cloud'\n[scenarios.chat]\nprovider='cloud'\nmodel='never-send-local'", server.base)).unwrap();
    let mut streamed = String::new();
    let result = crate::ask(
        &library,
        Some(&meeting),
        "Что с выпуском?",
        &[],
        &config,
        crate::Cancellation::default(),
        &mut |text| streamed.push_str(text),
    )
    .unwrap();
    assert_eq!(result.turn.mode, Mode::Local);
    assert_eq!(result.turn.answer, streamed);
    assert_eq!(result.citations[0].meeting.path(), meeting.path());
    let (_, body) = server.handle.join().unwrap();
    assert_eq!(body["model"], "local-chat");
    let prompt = body["messages"][1]["content"].as_str().unwrap();
    assert!(prompt.contains("Участники: Иван"));
    assert!(prompt.contains("Иван: готово"));
    assert!(prompt.contains("[1: План,"));
}
