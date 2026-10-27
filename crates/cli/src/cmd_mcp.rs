//! A small read-only MCP stdio server; stdout contains JSON-RPC messages only.
use std::{
    fs,
    io::{BufRead, Write},
    path::PathBuf,
};

use anyhow::{Context, Result, ensure};
use clap::Args;
use notetaker_core::{Library, Meeting, Mode};
use serde::Deserialize;
use serde_json::{Value, json};

#[derive(Args)]
pub struct Options {
    #[arg(long)]
    library: Option<PathBuf>,
}
#[derive(Deserialize)]
struct Preferences {
    library: Option<PathBuf>,
    #[serde(default = "exclude_local")]
    mcp_exclude_local: bool,
}
fn exclude_local() -> bool {
    true
}

pub fn run(options: Options) -> Result<()> {
    let settings = directories::ProjectDirs::from("", "", "notetaker")
        .context("Нет каталога настроек")?
        .config_dir()
        .join("app.toml");
    let prefs: Preferences = match fs::read_to_string(settings) {
        Ok(text) => toml::from_str(&text).context("Некорректный app.toml; MCP не запущен")?,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Preferences {
            library: None,
            mcp_exclude_local: true,
        },
        Err(error) => return Err(error.into()),
    };
    let root = options.library.or(prefs.library).unwrap_or_else(|| {
        let home = directories::UserDirs::new();
        home.as_ref()
            .map(|d| d.document_dir().unwrap_or(d.home_dir()).to_owned())
            .unwrap_or_else(|| PathBuf::from("."))
            .join("notetaker")
    });
    let server = Server {
        library: Library::open(root)?,
        exclude_local: prefs.mcp_exclude_local,
    };
    serve(&server, std::io::stdin().lock(), std::io::stdout().lock())
}

struct Server {
    library: Library,
    exclude_local: bool,
}
impl Server {
    fn allowed(&self, meeting: &Meeting) -> Result<bool> {
        // No arbitrary paths or symlinks outside the configured library.
        let path = meeting.path().canonicalize()?;
        Ok(path.parent() == Some(self.library.root())
            && meeting.validate_source_paths().is_ok()
            && (!self.exclude_local || meeting.read_meta()?.mode != Mode::Local))
    }
    fn call(&self, name: &str, args: Value) -> Result<Value> {
        match name {
            "list_tasks" => {
                #[derive(Deserialize)]
                #[serde(deny_unknown_fields)]
                struct Input {
                    status: Option<String>,
                    owner: Option<String>,
                }
                let input: Input = serde_json::from_value(args)?;
                let completed = match input.status.as_deref() {
                    None | Some("all") => None,
                    Some("open") => Some(false),
                    Some("completed") | Some("done") => Some(true),
                    _ => anyhow::bail!("status: open, completed или all"),
                };
                self.library.reindex_incremental()?;
                Ok(serde_json::to_value(self.library.list_tasks(
                    completed,
                    input.owner.as_deref(),
                    self.exclude_local,
                )?)?)
            }

            "search_meetings" => {
                #[derive(Deserialize)]
                #[serde(deny_unknown_fields)]
                struct Input {
                    query: String,
                    #[serde(default = "ten")]
                    limit: usize,
                }
                let input: Input = serde_json::from_value(args)?;
                ensure!(input.query.len() <= 16_384, "Слишком длинный запрос");
                self.library.reindex_incremental()?;
                let hits = self.library.retrieve(
                    &input.query,
                    input.limit.min(100),
                    self.exclude_local,
                )?;
                let available = self.library.list_meetings()?;
                let mut result = Vec::new();
                for hit in hits {
                    if let Some(meeting) = available.iter().find(|m| m.path() == hit.path)
                        && self.allowed(meeting)?
                    {
                        result.push(
                            json!({"id":hit.meeting_id,"title":hit.title,"snippet":hit.snippet}),
                        );
                    }
                }
                Ok(json!(result))
            }
            "list_recent" => {
                #[derive(Deserialize)]
                #[serde(deny_unknown_fields)]
                struct Input {
                    #[serde(default = "ten")]
                    n: usize,
                }
                let input: Input = serde_json::from_value(args)?;
                let mut result = Vec::new();
                for meeting in self.library.list_meetings()? {
                    if !self.allowed(&meeting)? {
                        continue;
                    }
                    let meta = meeting.read_meta()?;
                    result.push((
                        meta.started_at,
                        json!({"id":meta.id,"title":meta.title,"started_at":meta.started_at}),
                    ));
                }
                result.sort_by_key(|(date, _)| std::cmp::Reverse(*date));
                result.truncate(input.n.min(100));
                Ok(json!(
                    result.into_iter().map(|(_, row)| row).collect::<Vec<_>>()
                ))
            }
            "get_meeting" | "get_speaker_timeline" => {
                let timeline_meeting = if name == "get_speaker_timeline" {
                    #[derive(Deserialize)]
                    #[serde(deny_unknown_fields)]
                    struct TimelineInput {
                        meeting: String,
                    }
                    let input = TimelineInput::deserialize(&args)?;
                    Some(input.meeting)
                } else {
                    None
                };
                #[derive(Deserialize)]
                #[serde(deny_unknown_fields)]
                struct Input {
                    id: Option<String>,
                    path: Option<PathBuf>,
                }
                let input: Input = if let Some(meeting) = &timeline_meeting {
                    Input {
                        id: Some(meeting.clone()),
                        path: None,
                    }
                } else {
                    serde_json::from_value(args)?
                };
                ensure!(
                    input.id.is_some() != input.path.is_some(),
                    "Укажите id или path"
                );
                let requested_path = input
                    .path
                    .or_else(|| timeline_meeting.map(PathBuf::from))
                    .map(|p| {
                        if p.is_absolute() {
                            p
                        } else {
                            self.library.root().join(p)
                        }
                    })
                    .map(fs::canonicalize)
                    .transpose()
                    .ok()
                    .flatten();
                for meeting in self.library.list_meetings()? {
                    if !self.allowed(&meeting)? {
                        continue;
                    }
                    let meta = meeting.read_meta()?;
                    if input.id.as_deref() == Some(&meta.id.to_string())
                        || requested_path.as_deref() == Some(meeting.path())
                    {
                        // Check privacy again immediately before reading content.
                        ensure!(self.allowed(&meeting)?, "Встреча недоступна");
                        if name == "get_speaker_timeline" {
                            return Ok(serde_json::to_value(
                                meeting.read_transcript()?.speaker_timeline(),
                            )?);
                        }
                        return Ok(
                            json!({"meta":meta,"notes":meeting.read_notes()?,"summary":meeting.read_enhanced()?,"transcript":meeting.read_transcript()?.named_text()}),
                        );
                    }
                }
                anyhow::bail!("Встреча не найдена или недоступна по политике MCP")
            }
            _ => anyhow::bail!("Неизвестный инструмент"),
        }
    }
}
fn ten() -> usize {
    10
}
fn tools() -> Value {
    json!({"tools":[
        {"name":"list_tasks","description":"Задачи из конспектов. Соблюдает фильтр Local.","inputSchema":{"type":"object","properties":{"status":{"type":"string","enum":["all","open","completed","done"]},"owner":{"type":"string"}},"additionalProperties":false},"annotations":{"readOnlyHint":true,"openWorldHint":false}},
        {"name":"search_meetings","description":"Поиск встреч по словам, BM25. Local-встречи скрыты по настройке.","inputSchema":{"type":"object","properties":{"query":{"type":"string"},"limit":{"type":"integer","minimum":0,"maximum":100}},"required":["query"],"additionalProperties":false},"annotations":{"readOnlyHint":true,"openWorldHint":false}},
        {"name":"get_meeting","description":"Заметки, конспект и транскрипт с именами. Укажите id или path внутри библиотеки.","inputSchema":{"type":"object","properties":{"id":{"type":"string"},"path":{"type":"string"}},"additionalProperties":false},"annotations":{"readOnlyHint":true,"openWorldHint":false}},
        {"name":"get_speaker_timeline","description":"Таймлайн спикеров, суммарное время, доли, реплики и монологи. meeting: UUID или путь внутри библиотеки. Политика Local применяется.","inputSchema":{"type":"object","properties":{"meeting":{"type":"string"}},"required":["meeting"],"additionalProperties":false},"annotations":{"readOnlyHint":true,"openWorldHint":false}},
        {"name":"list_recent","description":"Последние доступные встречи.","inputSchema":{"type":"object","properties":{"n":{"type":"integer","minimum":0,"maximum":100}},"additionalProperties":false},"annotations":{"readOnlyHint":true,"openWorldHint":false}}
    ]})
}
fn error(id: Value, code: i32, message: &str) -> Value {
    json!({"jsonrpc":"2.0","id":id,"error":{"code":code,"message":message}})
}
fn handle(server: &Server, request: Value, initialized: &mut bool) -> Option<Value> {
    if request.get("jsonrpc") != Some(&json!("2.0"))
        || !request["method"].is_string()
        || request
            .get("id")
            .is_some_and(|id| !id.is_string() && !id.is_number() && !id.is_null())
    {
        return Some(error(Value::Null, -32600, "Invalid Request"));
    }
    let method = request["method"].as_str().unwrap();
    let id = request.get("id")?.clone(); // Notifications never produce responses.
    let params = request.get("params").cloned().unwrap_or_else(|| json!({}));
    if !params.is_object() {
        return Some(error(id, -32602, "Invalid params"));
    }
    let result = match method {
        "initialize" => {
            let version = params["protocolVersion"]
                .as_str()
                .filter(|version| ["2024-11-05", "2025-03-26", "2025-06-18"].contains(version))
                .unwrap_or("2025-06-18");
            *initialized = true;
            json!({"protocolVersion":version,"capabilities":{"tools":{}},"serverInfo":{"name":"notetaker","version":env!("CARGO_PKG_VERSION")}})
        }
        "ping" => json!({}),
        _ if !*initialized => return Some(error(id, -32002, "Server not initialized")),
        "tools/list" => tools(),
        "tools/call" => {
            let Some(name) = params["name"].as_str() else {
                return Some(error(id, -32602, "Missing tool name"));
            };
            if ![
                "list_tasks",
                "search_meetings",
                "get_meeting",
                "get_speaker_timeline",
                "list_recent",
            ]
            .contains(&name)
            {
                return Some(error(id, -32602, "Unknown tool"));
            }
            let args = params
                .get("arguments")
                .cloned()
                .unwrap_or_else(|| json!({}));
            match server.call(name, args) {
                Ok(data) => {
                    json!({"content":[{"type":"text","text":data.to_string()}],"isError":false})
                }
                Err(error_value) if error_value.downcast_ref::<serde_json::Error>().is_some() => {
                    return Some(error(id, -32602, "Invalid tool arguments"));
                }
                // Do not expose filesystem errors or file content in protocol errors.
                Err(_) => {
                    json!({"content":[{"type":"text","text":"Не удалось прочитать встречу: проверьте аргументы, библиотеку и политику Local в настройках MCP."}],"isError":true})
                }
            }
        }
        _ => return Some(error(id, -32601, "Method not found")),
    };
    Some(json!({"jsonrpc":"2.0","id":id,"result":result}))
}
fn serve(server: &Server, mut input: impl BufRead, mut output: impl Write) -> Result<()> {
    let mut initialized = false;
    loop {
        let mut line = Vec::new();
        let count =
            std::io::Read::take(&mut input, 1024 * 1024 + 1).read_until(b'\n', &mut line)?;
        if count == 0 {
            break;
        }
        ensure!(count <= 1024 * 1024, "MCP request exceeds 1 MiB");
        let response = match serde_json::from_slice(&line) {
            Ok(request) => handle(server, request, &mut initialized),
            Err(_) => Some(error(Value::Null, -32700, "Parse error")),
        };
        if let Some(response) = response {
            serde_json::to_writer(&mut output, &response)?;
            output.write_all(b"\n")?;
            output.flush()?;
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use notetaker_core::{Channel, MeetingMeta, Segment, Source, SpeakerId, Transcript};
    use std::io::Cursor;
    #[cfg(unix)]
    #[test]
    fn tasks_recheck_source_privacy() {
        let root = tempfile::tempdir().unwrap();
        let library = Library::open(root.path()).unwrap();
        let meeting = library
            .create_meeting(notetaker_core::MeetingMeta::new(
                "Secret",
                chrono::Utc::now(),
            ))
            .unwrap();
        meeting
            .write_enhanced("- [ ] Secret task - Иван - завтра")
            .unwrap();
        let server = Server {
            library,
            exclude_local: true,
        };
        assert_eq!(server.call("list_tasks", json!({})).unwrap(), json!([]));
        let mut meta = meeting.read_meta().unwrap();
        meta.mode = Mode::Hybrid;
        meeting.write_meta(&meta).unwrap();
        assert_eq!(
            server
                .call("list_tasks", json!({"status":"open","owner":"иван"}))
                .unwrap()
                .as_array()
                .unwrap()
                .len(),
            1
        );
        let response = handle(&server, json!({"jsonrpc":"2.0","id":1,"method":"tools/call","params":{"name":"list_tasks","arguments":{"status":"open"}}}), &mut true).unwrap();
        assert_eq!(response["result"]["isError"], false);
        meta.mode = Mode::Local;
        meeting.write_meta(&meta).unwrap();
        assert!(
            server
                .library
                .list_tasks(None, None, true)
                .unwrap()
                .is_empty()
        );
        assert!(
            server
                .call("list_tasks", json!({"status":"invalid"}))
                .is_err()
        );
    }
    #[test]
    fn all_tools_refuse_sources_linked_outside_the_specific_meeting() {
        use std::os::unix::fs::symlink;
        let root = tempfile::tempdir().unwrap();
        let library = Library::open(root.path()).unwrap();
        let local = library
            .create_meeting(MeetingMeta::new("SECRET", chrono::Utc::now()))
            .unwrap();
        local.write_notes("privateword").unwrap();
        let mut meta = MeetingMeta::new("Hybrid", chrono::Utc::now());
        meta.mode = Mode::Hybrid;
        let public = library.create_meeting(meta).unwrap();
        public.write_notes("publicword").unwrap();
        let id = public.read_meta().unwrap().id.to_string();
        let server = Server {
            library,
            exclude_local: true,
        };
        // Start with cached content, then replace each source independently.
        for name in ["notes.md", "enhanced.md", "transcript.json", "meta.yaml"] {
            server.library.reindex_full().unwrap();
            let path = public.path().join(name);
            let original = fs::read(&path).unwrap();
            fs::remove_file(&path).unwrap();
            symlink(local.path().join(name), &path).unwrap();
            assert!(
                server.call("get_meeting", json!({"id":id})).is_err(),
                "{name}"
            );
            assert!(
                server
                    .call("get_speaker_timeline", json!({"meeting":id}))
                    .is_err(),
                "{name}"
            );
            assert_eq!(
                server.call("list_recent", json!({})).unwrap(),
                json!([]),
                "{name}"
            );
            assert_eq!(
                server
                    .call("search_meetings", json!({"query":"privateword publicword"}))
                    .unwrap(),
                json!([]),
                "{name}"
            );
            assert!(
                server
                    .library
                    .retrieve("privateword publicword", 10, true)
                    .unwrap()
                    .is_empty()
            );
            fs::remove_file(path).unwrap();
            fs::write(public.path().join(name), original).unwrap();
        }
        let secret = root.path().join("voices.json");
        fs::write(&secret, "privateword").unwrap();
        fs::remove_file(public.path().join("notes.md")).unwrap();
        symlink(secret, public.path().join("notes.md")).unwrap();
        assert!(public.read_notes().is_err());
        assert!(server.call("get_meeting", json!({"id":id})).is_err());
        server.library.reindex_full().unwrap();
        assert!(
            server
                .library
                .search("privateword", 10)
                .unwrap()
                .iter()
                .all(|hit| hit.path == local.path())
        );
    }

    #[test]
    fn json_rpc_roundtrip_and_privacy_for_all_tools() {
        let root = tempfile::tempdir().unwrap();
        let library = Library::open(root.path()).unwrap();
        let now = chrono::Utc::now();
        let local = library
            .create_meeting(MeetingMeta::new("SECRET", now))
            .unwrap();
        local.write_notes("секрет Kubernetes").unwrap();
        let mut meta = MeetingMeta::new("План", now);
        meta.mode = Mode::Hybrid;
        let public = library.create_meeting(meta).unwrap();
        public.write_notes("Kubernetes").unwrap();
        let mut transcript = Transcript::default();
        transcript.rename_speaker("s1", "Иван");
        transcript.segments.push(Segment {
            id: public.read_meta().unwrap().id,
            speaker: SpeakerId("s1".into()),
            channel: Channel::System,
            start_ms: 0,
            end_ms: 1000,
            text: "Готово".into(),
            source: Source::Final,
            edited: false,
        });
        public.write_transcript(&transcript).unwrap();
        let id = public.read_meta().unwrap().id;
        let hidden = local.read_meta().unwrap().id;
        let privacy_server = Server {
            library: library.clone(),
            exclude_local: true,
        };
        let timeline = privacy_server
            .call("get_speaker_timeline", json!({"meeting":id}))
            .unwrap();
        assert_eq!(timeline["speakers"][0]["name"], "Иван");
        assert_eq!(timeline["speakers"][0]["total_ms"], 1000);
        assert!(
            privacy_server
                .call("get_speaker_timeline", json!({"meeting":hidden}))
                .is_err()
        );
        assert!(
            privacy_server
                .call("get_speaker_timeline", json!({"meeting":local.path()}))
                .is_err()
        );
        assert!(
            privacy_server
                .call("get_speaker_timeline", json!({"meeting":"../outside"}))
                .is_err()
        );
        let requests = [
            json!({"jsonrpc":"2.0","id":1,"method":"initialize","params":{"protocolVersion":"2025-06-18"}}),
            json!({"jsonrpc":"2.0","method":"notifications/initialized"}),
            json!({"jsonrpc":"2.0","id":2,"method":"tools/list"}),
            json!({"jsonrpc":"2.0","id":3,"method":"tools/call","params":{"name":"search_meetings","arguments":{"query":"Kubernetes"}}}),
            json!({"jsonrpc":"2.0","id":4,"method":"tools/call","params":{"name":"list_recent"}}),
            json!({"jsonrpc":"2.0","id":5,"method":"tools/call","params":{"name":"get_meeting","arguments":{"id":id}}}),
            json!({"jsonrpc":"2.0","id":6,"method":"tools/call","params":{"name":"get_meeting","arguments":{"id":hidden}}}),
            json!({"jsonrpc":"2.0","id":7,"method":"tools/call","params":{"name":"get_meeting","arguments":{"path":local.path()}}}),
            json!({"jsonrpc":"2.0","id":8,"method":"tools/call","params":{"name":"get_meeting","arguments":{"path":"../outside"}}}),
            json!({"jsonrpc":"2.0","id":9,"method":"bad"}),
        ];
        let input = requests
            .iter()
            .map(Value::to_string)
            .collect::<Vec<_>>()
            .join("\n")
            + "\n{bad}\n";
        let mut output = Vec::new();
        let server = Server {
            library,
            exclude_local: true,
        };
        serve(&server, Cursor::new(input), &mut output).unwrap();
        let text = String::from_utf8(output).unwrap();
        assert!(!text.contains("SECRET"));
        let responses: Vec<Value> = text
            .lines()
            .map(|line| serde_json::from_str(line).unwrap())
            .collect();
        assert_eq!(responses.len(), 10);
        assert_eq!(responses[1]["result"]["tools"].as_array().unwrap().len(), 5);
        for response in &responses[2..5] {
            assert_eq!(response["result"]["isError"], false);
        }
        assert!(
            responses[4]["result"]["content"][0]["text"]
                .as_str()
                .unwrap()
                .contains("Иван: Готово")
        );
        for response in &responses[5..8] {
            assert_eq!(response["result"]["isError"], true);
        }
        assert_eq!(responses[8]["error"]["code"], -32601);
        assert_eq!(responses[9]["error"]["code"], -32700);
        let server = Server {
            exclude_local: false,
            ..server
        };
        assert!(server.call("get_meeting", json!({"id":hidden})).is_ok());
        assert!(
            server
                .call("get_speaker_timeline", json!({"meeting":hidden}))
                .is_ok()
        );
    }
}
