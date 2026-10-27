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

fn serial() -> std::sync::MutexGuard<'static, ()> {
    static LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());
    LOCK.lock().unwrap_or_else(|poisoned| poisoned.into_inner())
}

#[test]
fn enhance_cli_roundtrip_with_both_providers_and_map_reduce() {
    let _serial = serial();
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

#[test]
fn ctrl_c_cancels_cli_provider_and_terminates_its_process_group() {
    use std::os::unix::{fs::PermissionsExt, process::CommandExt};
    let _serial = serial();
    let dir = tempfile::tempdir().unwrap();
    let library = notetaker_core::Library::open(dir.path().join("library")).unwrap();
    let mut meta = notetaker_core::MeetingMeta::new("Cancel", chrono::Utc::now());
    meta.mode = notetaker_core::Mode::Hybrid;
    let meeting = library.create_meeting(meta).unwrap();
    meeting.write_notes("notes").unwrap();
    meeting.write_enhanced("keep summary").unwrap();
    let config_dir = dir.path().join("config/notetaker");
    fs::create_dir_all(&config_dir).unwrap();
    let script = dir.path().join("claude");
    fs::write(&script, "#!/bin/sh\ncat >/dev/null\nsleep 30 &\necho $! > \"$(dirname \"$0\")/descendant\"\necho $$ > \"$(dirname \"$0\")/ready\"\nwait\n").unwrap();
    fs::set_permissions(&script, fs::Permissions::from_mode(0o700)).unwrap();
    fs::write(
        config_dir.join("config.toml"),
        "[defaults]\nlocal='cli'\nhybrid='cli'\n[[providers]]\nid='cli'\nkind='claude-cli'\n",
    )
    .unwrap();
    let mut paths = vec![dir.path().to_owned()];
    paths.extend(std::env::split_paths(&std::env::var_os("PATH").unwrap()));
    let mut child = Command::new(env!("CARGO_BIN_EXE_notetaker"))
        .arg("enhance")
        .arg(meeting.path())
        .env("XDG_CONFIG_HOME", dir.path().join("config"))
        .env("PATH", std::env::join_paths(paths).unwrap())
        .process_group(0)
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn()
        .unwrap();
    let deadline = Instant::now() + Duration::from_secs(15);
    while !fs::read_to_string(dir.path().join("ready"))
        .is_ok_and(|pid| pid.trim().parse::<u32>().is_ok())
    {
        assert!(
            child.try_wait().unwrap().is_none(),
            "enhance exited before provider started"
        );
        assert!(Instant::now() < deadline, "provider startup timed out");
        thread::sleep(Duration::from_millis(10));
    }
    assert!(
        Command::new("kill")
            .args(["-INT", &child.id().to_string()])
            .status()
            .unwrap()
            .success()
    );
    let deadline = Instant::now() + Duration::from_secs(10);
    let status = loop {
        if let Some(status) = child.try_wait().unwrap() {
            break status;
        }
        assert!(Instant::now() < deadline, "enhance did not stop on Ctrl+C");
        thread::sleep(Duration::from_millis(10));
    };
    assert!(!status.success());
    assert_eq!(
        fs::read_to_string(meeting.path().join("enhanced.md")).unwrap(),
        "keep summary"
    );
    for name in ["ready", "descendant"] {
        let pid = fs::read_to_string(dir.path().join(name)).unwrap();
        // An orphaned grandchild can briefly be a zombie until init reaps it.
        let deadline = Instant::now() + Duration::from_secs(10);
        loop {
            match fs::read_to_string(format!("/proc/{}/stat", pid.trim())) {
                Ok(stat) if !stat.split_once(") ").unwrap().1.starts_with('Z') => {
                    assert!(Instant::now() < deadline, "provider process still running");
                    thread::sleep(Duration::from_millis(10));
                }
                _ => break,
            }
        }
    }
}
