//! Optional local search, with an explicit Hybrid-only remote alternative.
use crate::{Config, OsCredentialStore, ProviderKind, resolve_key};
use anyhow::{Context, Result, ensure};
use notetaker_core::{
    Embedder, Hit, Library, Mode, SearchFilter, SearchMode, SemanticProgress,
    embedding::LocalEmbedder,
};
use std::{
    io::Read,
    sync::{Mutex, OnceLock},
    time::Duration,
};

pub struct SearchOutcome {
    pub hits: Vec<Hit>,
    pub note: Option<String>,
}
struct RemoteEmbedder {
    client: reqwest::blocking::Client,
    url: String,
    model: String,
    id: String,
    key: Option<String>,
}
impl RemoteEmbedder {
    fn new(config: &Config) -> Result<Self> {
        let choice = &config.embeddings;
        ensure!(
            !choice.provider.is_empty() && !choice.model.trim().is_empty(),
            "Удалённые embeddings не настроены"
        );
        let provider = config.select(Mode::Hybrid, Some(&choice.provider))?;
        ensure!(
            provider.kind == ProviderKind::Openai,
            "Embeddings требуют OpenAI-совместимый API"
        );
        let url = format!("{}/embeddings", provider.base_url.trim_end_matches('/'));
        let id = format!("remote:{url}:{}", choice.model);
        Ok(Self {
            client: reqwest::blocking::Client::builder()
                .redirect(reqwest::redirect::Policy::none())
                .connect_timeout(Duration::from_secs(15))
                .timeout(Duration::from_secs(90))
                .build()?,
            url,
            model: choice.model.clone(),
            id,
            key: resolve_key(provider, &OsCredentialStore)?,
        })
    }
}
impl Embedder for RemoteEmbedder {
    fn model_id(&self) -> &str {
        &self.id
    }
    fn is_local(&self) -> bool {
        false
    }
    fn embed(&mut self, text: &str, _query: bool) -> notetaker_core::Result<Vec<f32>> {
        let result = (|| -> Result<Vec<f32>> {
            let mut request = self.client.post(&self.url).json(
                &serde_json::json!({"model":self.model,"input":text,"encoding_format":"float"}),
            );
            if let Some(key) = &self.key {
                request = request.bearer_auth(key);
            }
            let response = request
                .send()
                .map_err(|_| anyhow::anyhow!("Embeddings: ошибка соединения"))?;
            ensure!(
                response.status().is_success(),
                "Embeddings: HTTP {}",
                response.status().as_u16()
            );
            let mut bytes = Vec::new();
            response.take(1_048_577).read_to_end(&mut bytes)?;
            ensure!(bytes.len() <= 1_048_576, "Слишком большой ответ embeddings");
            let value: serde_json::Value =
                serde_json::from_slice(&bytes).context("Некорректный ответ embeddings")?;
            let data = value["data"].as_array().context("Нет data в embeddings")?;
            ensure!(
                data.len() == 1 && data[0]["index"].as_u64() == Some(0),
                "Некорректный порядок embeddings"
            );
            let vector: Vec<f32> = serde_json::from_value(data[0]["embedding"].clone())
                .context("Нет вектора embeddings")?;
            ensure!(
                !vector.is_empty() && vector.len() <= 8192 && vector.iter().all(|v| v.is_finite()),
                "Некорректный вектор embeddings"
            );
            Ok(vector)
        })();
        result.map_err(|e| notetaker_core::Error::Embedding(e.to_string()))
    }
}
/// Cache only the local process. It is killed on failure, then retried next time.
/// A busy indexing/query worker never blocks another query: FTS remains usable.
static LOCAL: OnceLock<Mutex<Option<LocalEmbedder>>> = OnceLock::new();
pub fn search(
    library: &Library,
    query: &str,
    limit: usize,
    mode: SearchMode,
    filter: &SearchFilter,
    config: Option<&Config>,
    allow_remote: bool,
) -> Result<SearchOutcome> {
    library.reindex_incremental()?;
    let keyword = || -> Result<SearchOutcome> {
        Ok(SearchOutcome {
            hits: library.retrieve_filtered(query, limit, filter)?,
            note: None,
        })
    };
    if mode == SearchMode::Keyword || query.trim().is_empty() || limit == 0 {
        return keyword();
    }
    if notetaker_core::embedding::installed() {
        if !library.semantic_ready(notetaker_core::embedding::MODEL_ID)? {
            let mut outcome = keyword()?;
            outcome.note=Some("Индекс по смыслу ещё не готов: обновите его в настройках или через notetaker reindex --semantic".into());
            return Ok(outcome);
        }
        if let Ok(mut worker) = LOCAL.get_or_init(|| Mutex::new(None)).try_lock() {
            if worker.is_none() {
                *worker = LocalEmbedder::start().ok();
            }
            if let Some(embedder) = worker.as_mut() {
                match library.search_filtered(query, limit, mode, filter, Some(embedder)) {
                    Ok(hits) => return Ok(SearchOutcome { hits, note: None }),
                    Err(_) => *worker = None,
                }
            }
        }
    } else if allow_remote
        && let Some(config) = config
        && !config.embeddings.provider.is_empty()
    {
        let mut embedder = match RemoteEmbedder::new(config) {
            Ok(embedder) => embedder,
            Err(error) => {
                let mut outcome = keyword()?;
                outcome.note = Some(error.to_string());
                return Ok(outcome);
            }
        };
        if !library.semantic_ready(embedder.model_id())? {
            let mut outcome = keyword()?;
            outcome.note =
                Some("Индекс по смыслу ещё не готов. Local-встречи ищутся только по словам".into());
            return Ok(outcome);
        }
        if let Ok(hits) = library.search_filtered(query, limit, mode, filter, Some(&mut embedder)) {
            return Ok(SearchOutcome {hits,note:Some("Local-встречи ищутся только по словам; удалённые embeddings доступны только для Hybrid".into())});
        }
    }
    let mut outcome = keyword()?;
    outcome.note = Some(
        "Поиск по словам: модель поиска по смыслу не установлена, занята или недоступна".into(),
    );
    Ok(outcome)
}
struct LazyLocal {
    inner: Option<LocalEmbedder>,
}
impl Embedder for LazyLocal {
    fn model_id(&self) -> &str {
        notetaker_core::embedding::MODEL_ID
    }
    fn is_local(&self) -> bool {
        true
    }
    fn embed(&mut self, text: &str, query: bool) -> notetaker_core::Result<Vec<f32>> {
        if self.inner.is_none() {
            self.inner = Some(LocalEmbedder::start()?);
        }
        self.inner
            .as_mut()
            .expect("started worker")
            .embed(text, query)
    }
}
/// Called on a dedicated background thread in the GUI, and explicitly by CLI.
/// Remote indexing requires an explicitly configured embedding scenario.
pub fn reindex(
    library: &Library,
    full: bool,
    config: Option<&Config>,
    progress: &mut dyn FnMut(SemanticProgress),
) -> Result<SemanticProgress> {
    if notetaker_core::embedding::installed() {
        let mut embedder = LazyLocal { inner: None };
        return Ok(library.reindex_semantic(&mut embedder, full, progress)?);
    }
    let mut embedder =
        RemoteEmbedder::new(config.context("Скачайте модель поиска по смыслу в настройках")?)?;
    Ok(library.reindex_semantic(&mut embedder, full, progress)?)
}
pub fn configured(config: Option<&Config>) -> bool {
    notetaker_core::embedding::installed()
        || config
            .is_some_and(|c| !c.embeddings.provider.is_empty() && !c.embeddings.model.is_empty())
}

#[cfg(test)]
mod tests {
    use super::*;
    use notetaker_core::MeetingMeta;
    use std::{
        io::{BufRead, BufReader, Write},
        net::TcpListener,
        thread,
    };
    #[test]
    fn remote_index_sends_only_hybrid_text_to_embeddings_endpoint() {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        listener.set_nonblocking(true).unwrap();
        let base = format!("http://{}/v1", listener.local_addr().unwrap());
        let config=Config::parse(&format!("[defaults]\nlocal='server'\nhybrid='server'\n[[providers]]\nid='server'\nkind='openai'\nbase_url='{base}'\nmodel='chat'\n[embeddings]\nprovider='server'\nmodel='embed'\n")).unwrap();
        let handle = thread::spawn(move || {
            let until = std::time::Instant::now() + Duration::from_secs(10);
            let mut socket = loop {
                match listener.accept() {
                    Ok((socket, _)) => break socket,
                    Err(e)
                        if e.kind() == std::io::ErrorKind::WouldBlock
                            && std::time::Instant::now() < until =>
                    {
                        thread::sleep(Duration::from_millis(5))
                    }
                    Err(e) => panic!("no embedding request: {e}"),
                }
            };
            socket.set_nonblocking(false).unwrap();
            socket
                .set_read_timeout(Some(Duration::from_secs(5)))
                .unwrap();
            let mut reader = BufReader::new(&socket);
            let mut first = String::new();
            reader.read_line(&mut first).unwrap();
            assert!(first.starts_with("POST /v1/embeddings "));
            let mut size = 0;
            loop {
                let mut line = String::new();
                assert!(reader.read_line(&mut line).unwrap() > 0);
                if line == "\r\n" {
                    break;
                }
                if let Some(n) = line.to_lowercase().strip_prefix("content-length:") {
                    size = n.trim().parse::<usize>().unwrap();
                }
            }
            let mut bytes = vec![0; size];
            reader.read_exact(&mut bytes).unwrap();
            let body: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
            assert_eq!(body["input"], "PUBLIC деньги");
            assert_eq!(body["model"], "embed");
            let body = r#"{"data":[{"index":0,"embedding":[1.0,0.0]}]}"#;
            write!(socket,"HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",body.len()).unwrap();
        });
        let dir = tempfile::tempdir().unwrap();
        let library = Library::open(dir.path()).unwrap();
        library
            .create_meeting(MeetingMeta::new("Private", chrono::Utc::now()))
            .unwrap()
            .write_notes("SECRET")
            .unwrap();
        let mut meta = MeetingMeta::new("Shared", chrono::Utc::now());
        meta.mode = Mode::Hybrid;
        library
            .create_meeting(meta)
            .unwrap()
            .write_notes("PUBLIC деньги")
            .unwrap();
        let mut embedder = RemoteEmbedder::new(&config).unwrap();
        let result = library
            .reindex_semantic(&mut embedder, false, &mut |_| {})
            .unwrap();
        assert_eq!(result.excluded_local, 1);
        assert_eq!(result.chunks, 1);
        handle.join().unwrap();
    }
    #[test]
    #[ignore = "requires notetaker-embed and downloaded local model"]
    fn real_local_semantic_retrieval() -> Result<()> {
        let dir = tempfile::tempdir()?;
        let library = Library::open(dir.path())?;
        let meeting =
            library.create_meeting(MeetingMeta::new("Квартальный план", chrono::Utc::now()))?;
        meeting.write_notes("Встреча команды\nговорили про деньги на Q1")?;
        library
            .create_meeting(MeetingMeta::new("Выходные", chrono::Utc::now()))?
            .write_notes("кот спит на окне, на улице солнечная погода")?;
        let start = std::time::Instant::now();
        reindex(&library, false, None, &mut |_| {})?;
        eprintln!("REAL_INDEX ms={:.2}", start.elapsed().as_secs_f64() * 1000.);
        assert!(library.retrieve("бюджет", 10, false)?.is_empty());
        let start = std::time::Instant::now();
        let result = search(
            &library,
            "где обсуждали бюджет",
            10,
            SearchMode::Hybrid,
            &SearchFilter::default(),
            None,
            false,
        )?;
        eprintln!(
            "REAL_SEARCH first_ms={:.2}",
            start.elapsed().as_secs_f64() * 1000.
        );
        assert_eq!(result.hits[0].meeting_id, meeting.read_meta()?.id);
        assert!(matches!(
            result.hits[0].anchor,
            Some(notetaker_core::Anchor::Notes { line: 1 })
        ));
        let start = std::time::Instant::now();
        let result = search(
            &library,
            "где обсуждали бюджет",
            10,
            SearchMode::Hybrid,
            &SearchFilter::default(),
            None,
            false,
        )?;
        eprintln!(
            "REAL_SEARCH warm_ms={:.2}",
            start.elapsed().as_secs_f64() * 1000.
        );
        assert_eq!(result.hits[0].meeting_id, meeting.read_meta()?.id);
        Ok(())
    }
}
