// XDG_CONFIG_HOME isolates the CLI's ProjectDirs config without changing global
// process environment. Other platforms exercise both HTTP providers in llm tests.
#![cfg(target_os = "linux")]

use std::{
    fs,
    io::{BufRead, BufReader, Read, Write},
    net::TcpListener,
    process::Command,
    thread,
    time::{Duration, Instant},
};

use serde_json::{Value, json};

#[test]
fn enhance_cli_roundtrip_with_both_providers_and_map_reduce() {
    for kind in ["openai", "anthropic"] {
        let directory = tempfile::tempdir().unwrap();
        let meeting = directory.path().join("library/meeting");
        fs::create_dir_all(&meeting).unwrap();
        fs::write(
            meeting.join("meta.yaml"),
            "id: 00000000-0000-7000-8000-000000000001\ntitle: Release\nstarted_at: 2026-10-26T15:30:00Z\nmode: local\n",
        )
        .unwrap();
        fs::write(meeting.join("notes.md"), "- Keep my release plan").unwrap();
        fs::write(meeting.join("enhanced.md"), "old summary").unwrap();
        fs::write(
            meeting.join("transcript.json"),
            serde_json::to_vec(&json!({
                "version": 1,
                "segments": [{
                    "id": "00000000-0000-7000-8000-000000000002",
                    "speaker": "me", "channel": "mic", "start_ms": 65000,
                    "end_ms": 120000, "text": "Обсудим релиз. ".repeat(300),
                    "source": "final", "edited": false
                }]
            }))
            .unwrap(),
        )
        .unwrap();
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        listener.set_nonblocking(true).unwrap();
        let address = listener.local_addr().unwrap();
        let config_dir = directory.path().join("config/notetaker");
        fs::create_dir_all(&config_dir).unwrap();
        fs::write(
            config_dir.join("config.toml"),
            format!(
                "[defaults]\nlocal = 'mock'\nhybrid = 'mock'\n[[providers]]\nid = 'mock'\nkind = '{kind}'\nbase_url = 'http://localhost:{}/v1'\nmodel = 'test-model'\napi_key_env = 'NOTETAKER_CLI_TEST_KEY'\ncontext_tokens = 1024\n",
                address.port()
            ),
        )
        .unwrap();
        let server = thread::spawn(move || {
            let deadline = Instant::now() + Duration::from_secs(15);
            let mut requests = 0;
            loop {
                let mut stream = loop {
                    match listener.accept() {
                        Ok((stream, _)) => break stream,
                        Err(error)
                            if error.kind() == std::io::ErrorKind::WouldBlock
                                && Instant::now() < deadline =>
                        {
                            thread::sleep(Duration::from_millis(5))
                        }
                        Err(error) => panic!("CLI mock server: {error}"),
                    }
                };
                stream
                    .set_read_timeout(Some(Duration::from_secs(5)))
                    .unwrap();
                let mut reader = BufReader::new(&stream);
                let mut size = 0;
                let mut headers = String::new();
                loop {
                    let mut line = String::new();
                    assert!(reader.read_line(&mut line).unwrap() > 0);
                    if line == "\r\n" {
                        break;
                    }
                    if let Some(length) = line.to_lowercase().strip_prefix("content-length:") {
                        size = length.trim().parse().unwrap();
                    }
                    headers.push_str(&line.to_lowercase());
                }
                let mut body = vec![0; size];
                reader.read_exact(&mut body).unwrap();
                let body: Value = serde_json::from_slice(&body).unwrap();
                let (system, user) = if kind == "openai" {
                    assert!(headers.contains("authorization: bearer test-key"));
                    (
                        &body["messages"][0]["content"],
                        &body["messages"][1]["content"],
                    )
                } else {
                    assert!(headers.contains("x-api-key: test-key"));
                    (&body["system"], &body["messages"][0]["content"])
                };
                assert!(user.as_str().unwrap().contains("Keep my release plan"));
                let final_call = system
                    .as_str()
                    .unwrap()
                    .contains("Produce the final enhanced");
                let text = if final_call {
                    "# Итоги\nРелиз согласован."
                } else {
                    "Release agreed."
                };
                let response = if kind == "openai" {
                    json!({"choices": [{"message": {"content": text}, "finish_reason": "stop"}]})
                } else {
                    json!({"content": [{"type": "text", "text": text}], "stop_reason": "end_turn"})
                }
                .to_string();
                write!(stream, "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{response}", response.len()).unwrap();
                requests += 1;
                if final_call {
                    return requests;
                }
            }
        });
        let output = Command::new(env!("CARGO_BIN_EXE_notetaker"))
            .arg("enhance")
            .arg(&meeting)
            .args(["--template", "general", "--provider", "mock"])
            .env("XDG_CONFIG_HOME", directory.path().join("config"))
            .env("NOTETAKER_CLI_TEST_KEY", "test-key")
            // A local-only client must bypass even an explicitly broken proxy.
            .env("HTTP_PROXY", "http://127.0.0.1:1")
            .env("HTTPS_PROXY", "http://127.0.0.1:1")
            .env("ALL_PROXY", "http://127.0.0.1:1")
            .env("NO_PROXY", "")
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        let expected = "# Итоги\nРелиз согласован.";
        assert_eq!(
            fs::read_to_string(meeting.join("enhanced.md")).unwrap(),
            expected
        );
        assert_eq!(
            String::from_utf8(output.stdout).unwrap().trim_end(),
            expected
        );
        assert!(server.join().unwrap() > 2);
    }
}
