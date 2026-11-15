use super::*;
use std::{
    io::{Read, Write},
    net::TcpListener,
    thread,
};
fn server(status: u16, body: Vec<u8>) -> (Client, thread::JoinHandle<String>) {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let address = listener.local_addr().unwrap();
    let task = thread::spawn(move || {
        let (mut stream, _) = listener.accept().unwrap();
        let mut bytes = Vec::new();
        loop {
            let mut byte = [0];
            stream.read_exact(&mut byte).unwrap();
            bytes.push(byte[0]);
            if bytes.ends_with(b"\r\n\r\n") {
                break;
            }
        }
        let header = format!(
            "HTTP/1.1 {status} Test\r\nContent-Length: {}\r\nLocation: http://127.0.0.1:1/secret-query\r\nConnection: close\r\n\r\n",
            body.len()
        );
        stream.write_all(header.as_bytes()).unwrap();
        let _ = stream.write_all(&body);
        String::from_utf8(bytes).unwrap()
    });
    (
        Client::build(
            reqwest::Url::parse(&format!("http://{address}")).unwrap(),
            "test-secret",
        )
        .unwrap(),
        task,
    )
}
#[test]
fn sends_auth_and_ignores_unknown_optional_fields() {
    let (client, task) = server(
        200,
        br#"{"key":"neutral-id","firstname":"Oleg","surname":"Sidorov","extra":true}"#.to_vec(),
    );
    assert_eq!(client.profile().unwrap().name(), "Oleg Sidorov");
    let request = task.join().unwrap().to_lowercase();
    assert!(request.starts_with("get /api/users/profile "));
    assert!(request.contains("x-auth-token: test-secret"));
}
#[test]
fn pending_transcript_is_a_typed_retryable_outcome() {
    let (client, task) = server(200, br#"{"status":"pending","tracks":[]}"#.to_vec());
    let error = client.transcript("neutral").unwrap_err();
    assert!(error.is::<super::super::TranscriptNotReady>());
    assert_eq!(error.to_string(), "Транскрипция Толка ещё не готова");
    task.join().unwrap();
}
#[test]
fn rejects_redirect_auth_rate_limit_and_malformed_shapes_without_secrets() {
    for (status, body, expected) in [
        (302, "{}", "302"),
        (401, "test-secret", "401"),
        (429, "test-secret", "50"),
        (200, "{}", "структура"),
        (200, "{\"recordings\":null}", "структура"),
    ] {
        let (client, task) = server(status, body.as_bytes().to_vec());
        let error = client.recordings("private-query", 0, 100).unwrap_err();
        let error = format!("{error:#}");
        assert!(error.contains(expected), "{error}");
        assert!(
            !error.contains("test-secret")
                && !error.contains("private-query")
                && !error.contains("secret-query")
        );
        task.join().unwrap();
    }
}
#[test]
fn caps_body_and_rejects_insecure_or_embedded_credentials() {
    let (client, task) = server(200, vec![b' '; LIMIT as usize + 1]);
    assert!(client.profile().unwrap_err().to_string().contains("16 МиБ"));
    task.join().unwrap();
    for url in [
        "http://example.org",
        "https://user:password@example.org",
        "https://example.org?secret=1",
        "https://example.org/path",
    ] {
        assert!(Client::new(url, "test-secret").is_err());
    }
}
#[test]
fn transcript_requires_word_shape_but_tolerates_optional_fields() {
    let value = serde_json::json!({"status":"complete","tracks":[{"speaker":{"isAnonymous":true,"anonymousId":"guest","anonymousName":"Анна Петрова"},"chunks":[{"startTimeOffsetInMillis":0,"endTimeOffsetInMillis":100,"words":[{"text":"привет","startTimeOffsetInMillis":10,"endTimeOffsetInMillis":90}]}]}]});
    let (client, task) = server(200, serde_json::to_vec(&value).unwrap());
    assert_eq!(client.transcript("id").unwrap().tracks.len(), 1);
    task.join().unwrap();
    let (client, task) = server(
        200,
        br#"{"status":"complete","tracks":[{"chunks":[]}]}"#.to_vec(),
    );
    assert!(client.transcript("id").is_err());
    task.join().unwrap();
}

#[test]
fn paging_deduplicates_equal_timestamps_and_stops_without_new_ids() {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let address = listener.local_addr().unwrap();
    let recording = |id: &str, status: &str| serde_json::json!({"id":id,"title":"Нейтральная встреча","createdDate":"2026-01-01T12:00:00Z","duration":100,"status":status});
    let mut first: Vec<_> = (0..99)
        .map(|n| recording(&format!("processing-{n}"), "processing"))
        .collect();
    first.push(recording("A", "complete"));
    let mut second = vec![recording("A", "complete"), recording("extra", "processing")];
    second.extend((0..98).map(|n| recording(&format!("processing-{n}"), "processing")));
    let pages = [first, second.clone(), second];
    let task = thread::spawn(move || {
        for (page, skip) in pages.into_iter().zip([0, 100, 200]) {
            let (mut stream, _) = listener.accept().unwrap();
            let mut request = Vec::new();
            while !request.ends_with(b"\r\n\r\n") {
                let mut byte = [0];
                stream.read_exact(&mut byte).unwrap();
                request.push(byte[0]);
            }
            let request = String::from_utf8(request).unwrap();
            assert!(
                request.contains(&format!("skip={skip}&top=100")),
                "{request}"
            );
            let body = serde_json::to_vec(&serde_json::json!({"recordings":page})).unwrap();
            write!(
                stream,
                "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                body.len()
            )
            .unwrap();
            stream.write_all(&body).unwrap();
        }
    });
    let client = Client::build(
        reqwest::Url::parse(&format!("http://{address}")).unwrap(),
        "test-secret",
    )
    .unwrap();
    let dir = tempfile::tempdir().unwrap();
    let library = notetaker_core::Library::open(dir.path()).unwrap();
    let start = "2026-01-01T12:00:00Z".parse().unwrap();
    let mut meta = notetaker_core::MeetingMeta::new("Тест", start);
    meta.ended_at = Some(start + chrono::Duration::seconds(100));
    let meeting = library.create_meeting(meta).unwrap();
    assert_eq!(
        crate::ktalk::select_recording(&client, &meeting, None)
            .unwrap()
            .id,
        "A"
    );
    task.join().unwrap();
}

#[test]
fn rejects_extreme_negative_reversed_and_over_day_word_and_chunk_timings() {
    for (start, end) in [
        (0, i64::MAX),
        (i64::MIN, 1),
        (-1, 10),
        (10, 9),
        (0, MAX_TIME_MS + 1),
    ] {
        for field in ["word", "chunk"] {
            let mut chunk = serde_json::json!({"startTimeOffsetInMillis":0,"endTimeOffsetInMillis":100,"words":[{"text":"проверка","startTimeOffsetInMillis":0,"endTimeOffsetInMillis":100}]});
            let target = if field == "word" {
                &mut chunk["words"][0]
            } else {
                &mut chunk
            };
            target["startTimeOffsetInMillis"] = start.into();
            target["endTimeOffsetInMillis"] = end.into();
            let response = serde_json::json!({"status":"complete","tracks":[{"speaker":{"isAnonymous":true,"anonymousId":"guest","anonymousName":"Анна Петрова"},"chunks":[chunk]}]});
            let (client, task) = server(200, serde_json::to_vec(&response).unwrap());
            let error = client.transcript("test").unwrap_err().to_string();
            assert!(
                error.contains("Некорректная структура") && error.contains("недопустимое время"),
                "{field}: {error}"
            );
            task.join().unwrap();
        }
    }
    // Inclusive boundaries, including zero-length timing, are valid API values.
    assert!(valid_time(0, 0).is_ok());
    assert!(valid_time(0, MAX_TIME_MS).is_ok());
}

#[test]
fn process_pacer_allows_at_most_forty_requests_in_a_rolling_minute() {
    use std::cell::Cell;
    let state = Mutex::new(None);
    let now = Cell::new(Instant::now());
    let mut sent = Vec::new();
    for _ in 0..81 {
        pace_requests(&state, || now.get(), |delay| now.set(now.get() + delay));
        sent.push(now.get());
    }
    for window in sent.windows(41) {
        assert!(window[40].duration_since(window[0]) > Duration::from_secs(60));
    }
    for pair in sent.windows(2) {
        assert!(pair[1].duration_since(pair[0]) >= REQUEST_GAP);
    }
}

#[test]
fn slow_body_stops_at_the_deadline_and_large_body_at_the_limit() {
    struct Drip;
    impl std::io::Read for Drip {
        fn read(&mut self, buffer: &mut [u8]) -> std::io::Result<usize> {
            buffer[0] = b' ';
            Ok(1)
        }
    }
    let error = super::read_body(Drip, std::time::Instant::now()).unwrap_err();
    assert!(error.to_string().contains("слишком медленно"));
    let far = std::time::Instant::now() + std::time::Duration::from_secs(3600);
    let error = super::read_body(std::io::repeat(b' '), far).unwrap_err();
    assert!(error.to_string().contains("16 МиБ"));
    assert_eq!(super::read_body(&b"{}"[..], far).unwrap(), b"{}");
}
