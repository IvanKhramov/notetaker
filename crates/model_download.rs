//! Shared by the two model workers; dependencies are already linked by both.
use anyhow::{Context, Result, ensure};
use std::{
    fs::{self, OpenOptions},
    io::{Read, Seek, SeekFrom, Write},
    path::Path,
    time::{Duration, Instant},
};

/// Lock before opening a partial file, and hold through verification and install.
pub fn lock(root: &Path, id: &str) -> Result<fs::File> {
    notetaker_runtime::file_lock::exclusive(
        &root.join(format!(".{id}.lock")),
        Duration::from_secs(5),
    )
    .with_context(|| format!("Не удалось получить блокировку модели {id}"))
}
pub fn staging(root: &Path, id: &str) -> Result<std::path::PathBuf> {
    let path = root.join(format!(".{id}.staging"));
    if path.exists() {
        fs::remove_dir_all(&path)?;
    }
    fs::create_dir(&path)?;
    Ok(path)
}

/// Retain the partial file on transport failure. Only append a validated 206 range.
pub fn download(url: &str, path: &Path, size: u64, id: &str, stall: Duration) -> Result<()> {
    let mut file = OpenOptions::new()
        .create(true)
        .truncate(false)
        .read(true)
        .write(true)
        .open(path)?;
    let mut received = file.metadata()?.len();
    ensure!(
        received <= size,
        "Частичный файл превышает размер модели: {}",
        path.display()
    );
    eprintln!("NOTETAKER_DOWNLOAD {id} {received} {size}");
    if received == size {
        return Ok(());
    }
    // The blocking client applies this timeout separately to send and each Read.
    // An async read_timeout body requires a Tokio reactor while being polled.
    let client = reqwest::blocking::Client::builder()
        .connect_timeout(Duration::from_secs(30))
        .timeout(stall)
        .build()?;
    let mut request = client.get(url);
    if received > 0 {
        request = request.header(reqwest::header::RANGE, format!("bytes={received}-"));
    }
    let mut response = request.send()?.error_for_status()?;
    if response.status() == reqwest::StatusCode::PARTIAL_CONTENT {
        let range = response
            .headers()
            .get(reqwest::header::CONTENT_RANGE)
            .context("Нет Content-Range")?
            .to_str()?;
        let prefix = format!("bytes {received}-");
        ensure!(
            range.starts_with(&prefix) && range.ends_with(&format!("/{size}")),
            "Сервер вернул неверный Content-Range"
        );
    } else {
        ensure!(
            response.status() == reqwest::StatusCode::OK,
            "Неожиданный HTTP-статус загрузки"
        );
        // A server may ignore Range. This response is the complete file, not its suffix.
        file.set_len(0)?;
        received = 0;
        eprintln!("NOTETAKER_DOWNLOAD {id} 0 {size}");
    }
    file.seek(SeekFrom::Start(received))?;
    let mut buffer = [0u8; 65536];
    let mut last = Instant::now();
    loop {
        let n = response.read(&mut buffer).map_err(|error| {
            let timeout = error.kind() == std::io::ErrorKind::TimedOut
                || error
                    .get_ref()
                    .and_then(|e| e.downcast_ref::<reqwest::Error>())
                    .is_some_and(reqwest::Error::is_timeout);
            if timeout {
                anyhow::anyhow!(
                    "Загрузка остановилась: {} с без данных. Проверьте сеть. {error}",
                    stall.as_secs()
                )
            } else {
                anyhow::Error::new(error)
            }
        })?;
        if n == 0 {
            break;
        }
        received += n as u64;
        ensure!(received <= size, "Размер загрузки превышает манифест");
        file.write_all(&buffer[..n])?;
        if last.elapsed() >= Duration::from_millis(250) || received == size {
            eprintln!("NOTETAKER_DOWNLOAD {id} {received} {size}");
            last = Instant::now();
        }
    }
    file.sync_all()?;
    ensure!(
        received == size,
        "Загрузка оборвалась: {received} из {size} байт; повтор продолжит файл"
    );
    Ok(())
}
pub fn discard_corrupt(path: &Path) -> Result<()> {
    fs::remove_file(path)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn staging_is_deterministic_and_can_only_be_replaced_under_its_lock() {
        let root = tempfile::tempdir().unwrap();
        let holder = lock(root.path(), "semantic").unwrap();
        let first = staging(root.path(), "semantic").unwrap();
        fs::write(first.join("model.onnx"), b"old verified model copy").unwrap();
        let error = notetaker_runtime::file_lock::exclusive(
            &root.path().join(".semantic.lock"),
            Duration::from_millis(30),
        )
        .unwrap_err();
        assert_eq!(error.kind(), std::io::ErrorKind::TimedOut);
        assert!(first.join("model.onnx").is_file());
        drop(holder);
        let _holder = lock(root.path(), "semantic").unwrap();
        let second = staging(root.path(), "semantic").unwrap();
        assert_eq!(first, second);
        assert_eq!(fs::read_dir(second).unwrap().count(), 0);
        assert_eq!(
            fs::read_dir(root.path())
                .unwrap()
                .filter(|entry| entry.as_ref().unwrap().file_type().unwrap().is_dir())
                .count(),
            1
        );
    }
    use std::{
        net::TcpListener,
        sync::{Arc, Mutex},
        thread,
    };
    fn server(
        response: &'static [u8],
        pause: bool,
    ) -> (String, Arc<Mutex<String>>, thread::JoinHandle<()>) {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let url = format!("http://{}/model", listener.local_addr().unwrap());
        let request = Arc::new(Mutex::new(String::new()));
        let copy = request.clone();
        let worker = thread::spawn(move || {
            let (mut stream, _) = listener.accept().unwrap();
            let mut bytes = Vec::new();
            let mut byte = [0];
            while stream.read_exact(&mut byte).is_ok() {
                bytes.push(byte[0]);
                if bytes.ends_with(b"\r\n\r\n") {
                    break;
                }
            }
            *copy.lock().unwrap() = String::from_utf8(bytes).unwrap();
            stream.write_all(response).unwrap();
            if pause {
                thread::sleep(Duration::from_millis(900));
            }
        });
        (url, request, worker)
    }
    #[test]
    fn honors_range_and_keeps_exact_prefix() {
        let root = tempfile::tempdir().unwrap();
        let path = root.path().join("part");
        fs::write(&path, b"abc").unwrap();
        let(url,request,worker)=server(b"HTTP/1.1 206 Partial Content\r\nContent-Length: 3\r\nContent-Range: bytes 3-5/6\r\n\r\ndef",false);
        download(&url, &path, 6, "neutral", Duration::from_secs(2)).unwrap();
        worker.join().unwrap();
        assert_eq!(fs::read(path).unwrap(), b"abcdef");
        assert!(
            request
                .lock()
                .unwrap()
                .to_lowercase()
                .contains("range: bytes=3-")
        );
    }
    #[test]
    fn ignored_range_replaces_the_partial_instead_of_appending() {
        let root = tempfile::tempdir().unwrap();
        let path = root.path().join("part");
        fs::write(&path, b"abc").unwrap();
        let (url, _, worker) = server(b"HTTP/1.1 200 OK\r\nContent-Length: 6\r\n\r\nabcdef", false);
        download(&url, &path, 6, "neutral", Duration::from_secs(2)).unwrap();
        worker.join().unwrap();
        assert_eq!(fs::read(path).unwrap(), b"abcdef");
    }
    #[test]
    fn active_download_can_outlast_the_stall_timeout() {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let url = format!("http://{}/model", listener.local_addr().unwrap());
        let worker = thread::spawn(move || {
            let (mut stream, _) = listener.accept().unwrap();
            let mut request = Vec::new();
            let mut byte = [0];
            while !request.ends_with(b"\r\n\r\n") {
                stream.read_exact(&mut byte).unwrap();
                request.push(byte[0]);
            }
            stream
                .write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 6\r\n\r\n")
                .unwrap();
            for byte in b"abcdef" {
                stream.write_all(&[*byte]).unwrap();
                thread::sleep(Duration::from_millis(100));
            }
        });
        let root = tempfile::tempdir().unwrap();
        let path = root.path().join("part");
        download(&url, &path, 6, "neutral", Duration::from_millis(300)).unwrap();
        worker.join().unwrap();
        assert_eq!(fs::read(path).unwrap(), b"abcdef");
    }
    #[test]
    fn stalled_body_fails_and_its_partial_is_resumable() {
        let root = tempfile::tempdir().unwrap();
        let path = root.path().join("part");
        let (url, _, worker) = server(b"HTTP/1.1 200 OK\r\nContent-Length: 6\r\n\r\nabc", true);
        let error = download(&url, &path, 6, "neutral", Duration::from_millis(300)).unwrap_err();
        assert!(error.to_string().contains("без данных"));
        worker.join().unwrap();
        assert_eq!(fs::read(&path).unwrap(), b"abc");
        let(url,_,worker)=server(b"HTTP/1.1 206 Partial Content\r\nContent-Length: 3\r\nContent-Range: bytes 3-5/6\r\n\r\ndef",false);
        download(&url, &path, 6, "neutral", Duration::from_secs(2)).unwrap();
        worker.join().unwrap();
        assert_eq!(fs::read(path).unwrap(), b"abcdef");
    }
}
