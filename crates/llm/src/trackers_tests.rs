use super::*;
use std::{
    io::{BufRead, BufReader, Read, Write},
    net::TcpListener,
    thread::{self, JoinHandle},
    time::{Duration, Instant},
};
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
}

struct Store;
impl CredentialStore for Store {
    fn read(&self, id: &str) -> Result<String> {
        // No saved origin: a key from before the origin binding.
        anyhow::ensure!(!id.starts_with("tracker-origin/"), "no entry");
        Ok("test-key".into())
    }
    fn write(&self, _: &str, _: &str) -> Result<()> {
        Ok(())
    }
}
fn fixture() -> (tempfile::TempDir, Library, Task) {
    let dir = tempfile::tempdir().unwrap();
    let l = Library::open(dir.path()).unwrap();
    let mut meta = notetaker_core::MeetingMeta::new("Weekly", chrono::Utc::now());
    meta.mode = Mode::Hybrid;
    let m = l.create_meeting(meta).unwrap();
    m.write_enhanced("- [ ] Deploy - ivan@example.org - tomorrow\n")
        .unwrap();
    l.reindex_incremental().unwrap();
    let task = l.list_tasks(None, None, false).unwrap().remove(0);
    (dir, l, task)
}
#[test]
fn providers_send_expected_requests_and_record_receipts() {
    for kind in [
        TrackerKind::Webhook,
        TrackerKind::Kaiten,
        TrackerKind::Jira,
        TrackerKind::YouTrack,
    ] {
        let response = match kind {
            TrackerKind::Webhook => r#"{"url":"https://issues.test/1"}"#,
            TrackerKind::Kaiten => r#"{"id":42}"#,
            TrackerKind::Jira => r#"{"key":"OPS-1"}"#,
            TrackerKind::YouTrack => r#"{"idReadable":"OPS-2"}"#,
        };
        let mock = Mock::start("201 Created", "application/json", response, "");
        let (_dir, l, task) = fixture();
        let config = TrackerConfig {
            kind,
            base_url: mock.base.clone(),
            credential_id: "test".into(),
            email: "me@example.org".into(),
            project: "OPS".into(),
            issue_type: "Task".into(),
            jira_v3: true,
            space_id: "1".into(),
            board_id: "2".into(),
            column_id: "3".into(),
            lane_id: "4".into(),
            owners: [("ivan@example.org".into(), 7)].into(),
            ..Default::default()
        };
        let preview = preview(&l, task, config).unwrap();
        let url = send(&l, &preview, false, &Store).unwrap();
        assert_eq!(
            send(&l, &preview, false, &Store).unwrap_err().to_string(),
            "Конспект изменился; создайте новый просмотр"
        );
        let (headers, body) = mock.handle.join().unwrap();
        let headers = headers.to_lowercase();
        assert!(headers.contains("idempotency-key:"));
        match kind {
            TrackerKind::Webhook => {
                assert!(headers.starts_with("post /v1 "));
                assert!(headers.contains("x-notetaker-signature: sha256="));
                assert_eq!(body["task"], "Deploy");
                assert_eq!(body["meeting"]["meeting_title"], "Weekly");
            }
            TrackerKind::Kaiten => {
                assert!(headers.starts_with("post /v1/api/latest/cards "));
                assert_eq!(body["board_id"], 2);
                assert_eq!(body["responsible_id"], 7);
                assert!(headers.contains("authorization: bearer test-key"));
            }
            TrackerKind::Jira => {
                assert!(headers.starts_with("post /v1/rest/api/3/issue "));
                assert_eq!(body["fields"]["description"]["type"], "doc");
                assert_eq!(body["fields"]["project"]["key"], "OPS");
                assert!(headers.contains("authorization: basic "));
            }
            TrackerKind::YouTrack => {
                assert!(headers.starts_with("post /v1/api/issues?fields=idreadable "));
                assert_eq!(body["project"]["id"], "OPS");
                assert_eq!(body["summary"], "Deploy");
            }
        }
        let sent = l.list_tasks(None, None, false).unwrap().remove(0);
        assert_eq!(sent.issue_url.as_deref(), Some(url.as_str()));
        assert!(super::preview(&l, sent, preview.config).is_err());
    }
}
#[test]
fn local_confirmation_is_checked_again_before_credentials_or_network() {
    let (_dir, l, task) = fixture();
    let meeting = task.meeting(&l).unwrap();
    let preview = preview(
        &l,
        task,
        TrackerConfig {
            base_url: "https://tracker.test/hook".into(),
            ..Default::default()
        },
    )
    .unwrap();
    meeting.update_meta(|m| m.mode = Mode::Local).unwrap();
    assert!(
        send(&l, &preview, false, &Store)
            .unwrap_err()
            .to_string()
            .contains("Подтвердите")
    );
    assert!(!meeting.path().read_dir().unwrap().any(|e| {
        e.unwrap()
            .file_name()
            .to_string_lossy()
            .starts_with(".tracker-")
    }));
}
#[test]
fn hmac_matches_rfc4231_and_jira_server_uses_text() {
    assert_eq!(
        signature(&[0x0b; 20], b"Hi There"),
        "sha256=b0344c61d8db38535ca8afceaf0bf12b881dc200c9833da726e9376c2e32cff7"
    );
    let (_dir, l, task) = fixture();
    let p = preview(
        &l,
        task,
        TrackerConfig {
            kind: TrackerKind::Jira,
            base_url: "https://jira.test".into(),
            project: "OPS".into(),
            issue_type: "Task".into(),
            ..Default::default()
        },
    )
    .unwrap();
    assert!(p.endpoint.ends_with("/rest/api/2/issue"));
    assert!(p.body["fields"]["description"].is_string());
}
#[test]
fn pending_receipt_blocks_retries_after_an_unknown_result() {
    let mock = Mock::start("503 Unavailable", "application/json", "{}", "");
    let (_dir, l, task) = fixture();
    let p = preview(
        &l,
        task,
        TrackerConfig {
            base_url: mock.base.clone(),
            ..Default::default()
        },
    )
    .unwrap();
    assert!(send(&l, &p, false, &Store).is_err());
    mock.handle.join().unwrap();
    assert!(
        send(&l, &p, false, &Store)
            .unwrap_err()
            .to_string()
            .contains("Предыдущая отправка")
    );
}

#[test]
fn explicit_local_consent_allows_one_request() {
    let mock = Mock::start(
        "201 Created",
        "application/json",
        r#"{"url":"https://issues.test/local"}"#,
        "",
    );
    let (_dir, l, task) = fixture();
    task.meeting(&l)
        .unwrap()
        .update_meta(|m| m.mode = Mode::Local)
        .unwrap();
    let p = preview(
        &l,
        task,
        TrackerConfig {
            base_url: mock.base.clone(),
            ..Default::default()
        },
    )
    .unwrap();
    assert!(p.local);
    assert!(send(&l, &p, false, &Store).is_err());
    assert_eq!(
        send(&l, &p, true, &Store).unwrap(),
        "https://issues.test/local"
    );
    mock.handle.join().unwrap();
}

#[test]
fn kaiten_destinations_use_bearer_and_explicit_pagination() {
    let mock = Mock::start(
        "200 OK",
        "application/json",
        r#"[{"id":1,"title":"Team"}]"#,
        "",
    );
    let choices = kaiten_destinations(
        &TrackerConfig {
            base_url: mock.base.clone(),
            credential_id: "test".into(),
            ..Default::default()
        },
        &Store,
    )
    .unwrap();
    assert_eq!(choices.spaces[0].title, "Team");
    let (headers, _) = mock.handle.join().unwrap();
    assert!(headers.starts_with("GET /v1/api/latest/spaces?limit=100&offset=0 "));
    assert!(
        headers
            .to_lowercase()
            .contains("authorization: bearer test-key")
    );
}

#[derive(Default)]
struct MemoryStore(std::sync::Mutex<std::collections::HashMap<String, String>>);
impl CredentialStore for MemoryStore {
    fn read(&self, id: &str) -> Result<String> {
        self.0
            .lock()
            .unwrap()
            .get(id)
            .cloned()
            .ok_or_else(|| anyhow::anyhow!("no entry"))
    }
    fn write(&self, id: &str, value: &str) -> Result<()> {
        self.0.lock().unwrap().insert(id.into(), value.into());
        Ok(())
    }
}
#[test]
fn tracker_key_is_released_only_to_the_origin_it_was_saved_for() {
    let store = MemoryStore::default();
    let mut config = TrackerConfig {
        kind: TrackerKind::Kaiten,
        base_url: "https://kaiten.example.org/".into(),
        credential_id: "work".into(),
        ..Default::default()
    };
    save_secret(&config, "token", &store).unwrap();
    assert_eq!(secret_for(&config, &store).unwrap(), "token");
    // A path change stays on the same origin.
    config.base_url = "https://kaiten.example.org/api".into();
    assert_eq!(secret_for(&config, &store).unwrap(), "token");
    // Another member of a shared library points the folder at their own host.
    config.base_url = "https://attacker.example.net".into();
    let error = secret_for(&config, &store).unwrap_err().to_string();
    assert!(error.contains("Адрес трекера изменился"));
    assert!(!error.contains("token"));

    // A key saved before the binding existed is bound by its first use.
    let legacy = MemoryStore::default();
    legacy.write("tracker/work", "token").unwrap();
    config.base_url = "https://kaiten.example.org".into();
    assert_eq!(secret_for(&config, &legacy).unwrap(), "token");
    config.base_url = "https://attacker.example.net".into();
    assert!(secret_for(&config, &legacy).is_err());
}
#[test]
fn shared_tracker_configuration_rejects_url_credentials_and_query_secrets() {
    for url in [
        "https://user:password@tracker.test/hook",
        "https://tracker.test/hook?token=secret",
        "file:///tmp/hook",
    ] {
        assert!(
            validate_config(&TrackerConfig {
                base_url: url.into(),
                ..Default::default()
            })
            .is_err()
        );
    }
    assert!(
        validate_config(&TrackerConfig {
            base_url: "https://tracker.test/hook".into(),
            ..Default::default()
        })
        .is_ok()
    );
}
