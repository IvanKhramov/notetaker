use std::{
    fs::{self, File},
    path::{Path, PathBuf},
    process::{Command, Stdio},
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
    thread,
    time::{Duration, Instant},
};

use anyhow::{Context, Result, ensure};
use notetaker_core::Mode;

use crate::{CompletionRequest, Provider, ProviderConfig, ProviderKind};

#[derive(Clone, Default)]
pub struct Cancellation(Arc<AtomicBool>);
impl Cancellation {
    pub fn cancel(&self) {
        self.0.store(true, Ordering::Relaxed);
    }
    pub fn is_cancelled(&self) -> bool {
        self.cancelled()
    }
    fn cancelled(&self) -> bool {
        self.0.load(Ordering::Relaxed)
    }
}

fn binary_name(kind: ProviderKind) -> &'static str {
    if kind == ProviderKind::ClaudeCli {
        "claude"
    } else {
        "codex"
    }
}

pub fn find_cli(kind: ProviderKind) -> Option<PathBuf> {
    find_in_path(kind, std::env::var_os("PATH").as_deref())
}

fn find_in_path(kind: ProviderKind, path: Option<&std::ffi::OsStr>) -> Option<PathBuf> {
    let name = binary_name(kind);
    let dirs: Vec<_> = path
        .map(std::env::split_paths)
        .into_iter()
        .flatten()
        .collect();
    #[cfg(windows)]
    let dirs = {
        let mut dirs = dirs;
        if let Some(home) = std::env::var_os("USERPROFILE") {
            dirs.push(PathBuf::from(home).join(".local/bin"));
        }
        if let Some(appdata) = std::env::var_os("APPDATA") {
            dirs.push(PathBuf::from(appdata).join("npm"));
        }
        dirs
    };
    for dir in dirs {
        #[cfg(windows)]
        let names = [
            format!("{name}.exe"),
            format!("{name}.cmd"),
            format!("{name}.bat"),
        ];
        #[cfg(not(windows))]
        let names = [name.to_owned()];
        for file in names {
            let file = dir.join(file);
            if file.is_file() {
                #[cfg(unix)]
                {
                    use std::os::unix::fs::PermissionsExt;
                    if fs::metadata(&file).ok()?.permissions().mode() & 0o111 == 0 {
                        continue;
                    }
                }
                // Keep normal Windows paths: cmd shims do not accept verbatim paths reliably.
                #[cfg(windows)]
                return Some(if file.is_absolute() {
                    file
                } else {
                    std::env::current_dir().ok()?.join(file)
                });
                #[cfg(not(windows))]
                return fs::canonicalize(file).ok();
            }
        }
    }
    None
}

fn background(binary: &Path) -> Command {
    let mut command = Command::new(binary);
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        command.creation_flags(0x08000000); // CREATE_NO_WINDOW
    }
    command
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null());
    command
}

// These flags are required: older CLIs must fail closed instead of silently
// falling back to the user's tools, MCP servers, hooks or project settings.
fn claude_isolation(command: &mut Command) {
    command.args([
        "--tools",
        "",
        "--strict-mcp-config",
        "--setting-sources",
        "",
        "--disable-slash-commands",
        "--no-session-persistence",
    ]);
}

fn codex_isolation(command: &mut Command) {
    command.args(["--ignore-user-config", "--ignore-rules", "--ephemeral"]);
    for setting in [
        "features.shell_tool=false",
        "web_search=\"disabled\"",
        "mcp_servers={}",
        "features.apps=false",
        "features.remote_plugin=false",
        "features.multi_agent=false",
        "features.hooks=false",
        "features.memories=false",
        "tools.view_image=false",
        "project_doc_max_bytes=0",
    ] {
        command.args(["-c", setting]);
    }
}

// File-backed stdin/stdout avoid pipe deadlocks for long transcripts and CLI logs.
fn run(
    mut command: Command,
    prompt: &str,
    dir: &Path,
    timeout: Duration,
    cancel: &Cancellation,
) -> Result<String> {
    ensure!(!cancel.cancelled(), "Создание конспекта отменено");
    let input_path = dir.join("stdin.txt");
    let output_path = dir.join("stdout.txt");
    fs::write(&input_path, prompt)?;
    let working_dir = dir.join("work");
    fs::create_dir(&working_dir)?;
    command
        .current_dir(working_dir)
        .stdin(File::open(input_path)?)
        .stdout(File::create(&output_path)?);
    #[cfg(unix)]
    {
        use std::os::unix::process::CommandExt;
        command.process_group(0);
    }
    let mut child = command
        .spawn()
        .context("Не удалось запустить CLI; проверьте установку")?;
    let started = Instant::now();
    let status = loop {
        if cancel.cancelled() || started.elapsed() >= timeout {
            terminate(&mut child);
            ensure!(!cancel.cancelled(), "Создание конспекта отменено");
            anyhow::bail!("CLI не ответил вовремя; проверьте вход и повторите запрос");
        }
        match child.try_wait() {
            Ok(Some(status)) => break status,
            Ok(None) => {}
            Err(error) => {
                terminate(&mut child);
                return Err(error).context("Не удалось дождаться CLI");
            }
        }
        thread::sleep(Duration::from_millis(50));
    };
    ensure!(
        status.success(),
        "CLI завершился с ошибкой. Проверьте вход, модель и лимиты подписки."
    );
    ensure!(
        fs::metadata(&output_path)?.len() <= 16 * 1024 * 1024,
        "Слишком большой ответ CLI"
    );
    fs::read_to_string(output_path).context("Некорректный вывод CLI")
}

fn terminate(child: &mut std::process::Child) {
    #[cfg(windows)]
    {
        let _ = background(Path::new("taskkill.exe"))
            .args(["/PID", &child.id().to_string(), "/T", "/F"])
            .status();
    }
    #[cfg(unix)]
    {
        // Each request starts a fresh process group, including npm wrapper children.
        #[allow(unsafe_code)]
        unsafe {
            libc::kill(-(child.id() as i32), libc::SIGKILL);
        }
    }
    let _ = child.kill();
    let _ = child.wait();
}

pub struct CliProvider {
    config: ProviderConfig,
    binary: PathBuf,
    cancel: Cancellation,
    timeout: Duration,
}
impl CliProvider {
    pub fn new(config: ProviderConfig, mode: Mode, cancel: Cancellation) -> Result<Self> {
        config.validate(mode)?;
        ensure!(config.kind.is_cli(), "Требуется CLI-провайдер");
        let binary =
            find_cli(config.kind).context("CLI не установлен; установите его и выполните вход")?;
        Ok(Self {
            config,
            binary,
            cancel,
            timeout: Duration::from_secs(600),
        })
    }
    pub fn check_completion(&self) -> Result<()> {
        let provider = Self {
            config: self.config.clone(),
            binary: self.binary.clone(),
            cancel: self.cancel.clone(),
            timeout: Duration::from_secs(30),
        };
        provider.complete(
            &CompletionRequest {
                system: "Reply briefly.".into(),
                user: "ok".into(),
                max_output_tokens: 16,
            },
            &mut |_| {},
        )?;
        Ok(())
    }
    pub fn check_login(&self) -> Result<()> {
        let dir = tempfile::tempdir()?;
        let mut command = background(&self.binary);
        if self.config.kind == ProviderKind::CodexCli {
            command.args(["login", "status"]);
            run(
                command,
                "",
                dir.path(),
                Duration::from_secs(30),
                &self.cancel,
            )?;
        } else {
            command.args(["-p", "ok", "--output-format", "json"]);
            claude_isolation(&mut command);
            if !self.config.model.is_empty() {
                command.arg("--model").arg(&self.config.model);
            }
            let response = run(
                command,
                "",
                dir.path(),
                Duration::from_secs(30),
                &self.cancel,
            )?;
            claude_result(&response)?;
        }
        Ok(())
    }
}
fn claude_result(response: &str) -> Result<String> {
    let value: serde_json::Value = serde_json::from_str(response)
        .map_err(|_| anyhow::anyhow!("Некорректный JSON Claude CLI"))?;
    ensure!(
        value["is_error"] != true,
        "Ошибка Claude CLI; проверьте вход и лимиты подписки"
    );
    let text = value["result"]
        .as_str()
        .context("Claude CLI не вернул result")?;
    ensure!(!text.trim().is_empty(), "CLI вернул пустой ответ");
    Ok(text.into())
}
impl Provider for CliProvider {
    fn config(&self) -> &ProviderConfig {
        &self.config
    }
    fn complete(
        &self,
        request: &CompletionRequest,
        on_text: &mut dyn FnMut(&str),
    ) -> Result<String> {
        let dir = tempfile::tempdir()?;
        let mut command = background(&self.binary);
        let final_path = dir.path().join("final.md");
        if self.config.kind == ProviderKind::ClaudeCli {
            command.args(["-p", "--output-format", "json"]);
            claude_isolation(&mut command);
        } else {
            command
                .args(["exec", "--skip-git-repo-check", "-s", "read-only", "-o"])
                .arg(&final_path)
                .arg("-");
            codex_isolation(&mut command);
        }
        if !self.config.model.is_empty() {
            command.arg("--model").arg(&self.config.model);
        }
        let prompt = format!(
            "{}\nDo not use tools. Limit the answer to approximately {} tokens.\n\n{}",
            request.system, request.max_output_tokens, request.user
        );
        let response = run(command, &prompt, dir.path(), self.timeout, &self.cancel)?;
        let text = if self.config.kind == ProviderKind::ClaudeCli {
            claude_result(&response)?
        } else {
            ensure!(
                fs::metadata(&final_path)?.len() <= 16 * 1024 * 1024,
                "Слишком большой ответ CLI"
            );
            fs::read_to_string(final_path).context("Codex CLI не сохранил итоговый ответ")?
        };
        ensure!(!text.trim().is_empty(), "CLI вернул пустой ответ");
        on_text(&text);
        Ok(text)
    }
}

#[cfg(any(windows, test))]
fn login_command(binary: &Path, kind: ProviderKind) -> Command {
    let mut command = Command::new(binary);
    if kind == ProviderKind::CodexCli {
        command.arg("login");
    }
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        command.creation_flags(0x00000010); // CREATE_NEW_CONSOLE
    }
    command
}

pub fn open_login(kind: ProviderKind) -> Result<()> {
    let binary = find_cli(kind).context("CLI не установлен")?;
    #[cfg(windows)]
    {
        login_command(&binary, kind)
            .spawn()
            .context("Не удалось открыть окно входа")?;
    }
    #[cfg(not(windows))]
    {
        let mut command = Command::new("x-terminal-emulator");
        command.arg("-e").arg(binary);
        if kind == ProviderKind::CodexCli {
            command.arg("login");
        }
        command
            .spawn()
            .context("Не удалось открыть терминал; выполните claude или codex login вручную")?;
    }
    Ok(())
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;
    use std::os::unix::fs::PermissionsExt;

    // Forks from parallel tests can briefly hold another test's freshly written
    // script open for writing, so exec fails with ETXTBSY. Run them serially.
    pub(super) fn serial() -> std::sync::MutexGuard<'static, ()> {
        static LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());
        LOCK.lock().unwrap_or_else(|poisoned| poisoned.into_inner())
    }

    fn fixture(kind: ProviderKind, script: &str) -> (tempfile::TempDir, CliProvider) {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join(binary_name(kind));
        fs::write(&path, format!("#!/bin/sh\n{script}\n")).unwrap();
        fs::set_permissions(&path, fs::Permissions::from_mode(0o700)).unwrap();
        // Resolve against an isolated PATH value, without mutating global process state.
        let binary = find_in_path(kind, Some(dir.path().as_os_str())).unwrap();
        let provider = CliProvider {
            config: ProviderConfig {
                id: "cli-test".into(),
                kind,
                base_url: String::new(),
                model: "sonnet".into(),
                api_key_env: None,
                key_source: Default::default(),
                context_tokens: 150_000,
            },
            binary,
            cancel: Cancellation::default(),
            timeout: Duration::from_secs(30),
        };
        (dir, provider)
    }
    fn request() -> CompletionRequest {
        CompletionRequest {
            system: "system marker".into(),
            user: "transcript marker".into(),
            max_output_tokens: 128,
        }
    }
    #[test]
    fn claude_stdin_json_and_no_bare_or_tools() {
        let _serial = serial();
        let (_dir, provider) = fixture(
            ProviderKind::ClaudeCli,
            r#"
[ "$1" = '-p' ] || exit 1
[ "$2" = '--output-format' ] || exit 1
[ "$3" = 'json' ] || exit 1
[ "$4" = '--tools' ] && [ "$5" = '' ] || exit 1
[ "$6" = '--strict-mcp-config' ] || exit 1
[ "$7" = '--setting-sources' ] && [ "$8" = '' ] || exit 1
[ "$9" = '--disable-slash-commands' ] || exit 1
shift 9
[ "$1" = '--no-session-persistence' ] || exit 1
[ "$2" = '--model' ] && [ "$3" = 'sonnet' ] || exit 1
input=$(cat)
case "$input" in *'system marker'*'transcript marker'*) ;; *) exit 1 ;; esac
printf '%s' '{"result":"summary","is_error":false}'
"#,
        );
        let mut streamed = String::new();
        assert_eq!(
            provider
                .complete(&request(), &mut |s| streamed.push_str(s))
                .unwrap(),
            "summary"
        );
        assert_eq!(streamed, "summary");
        assert!(
            provider
                .config
                .validate(Mode::Local)
                .unwrap_err()
                .to_string()
                .contains("Local")
        );
    }
    #[test]
    fn codex_uses_last_message_and_empty_read_only_workspace() {
        let _serial = serial();
        let (_dir, provider) = fixture(
            ProviderKind::CodexCli,
            r#"
[ "$1" = 'exec' ] && [ "$2" = '--skip-git-repo-check' ] || exit 1
[ "$3" = '-s' ] && [ "$4" = 'read-only' ] || exit 1
[ "$5" = '-o' ] || exit 1
output="$6"
shift 7
[ "$1" = '--ignore-user-config' ] || exit 1
[ "$2" = '--ignore-rules' ] && [ "$3" = '--ephemeral' ] || exit 1
shift 3
for setting in 'features.shell_tool=false' 'web_search="disabled"' 'mcp_servers={}' 'features.apps=false' 'features.remote_plugin=false' 'features.multi_agent=false' 'features.hooks=false' 'features.memories=false' 'tools.view_image=false' 'project_doc_max_bytes=0'; do
    [ "$1" = '-c' ] && [ "$2" = "$setting" ] || exit 1
    shift 2
done
[ -z "$(ls -A)" ] || exit 1
cat >/dev/null
printf '%s' 'last message' > "$output"
printf '%s' 'tool log must never be treated as final text'
"#,
        );
        assert_eq!(
            provider.complete(&request(), &mut |_| {}).unwrap(),
            "last message"
        );
    }
    #[test]
    fn cli_failures_empty_results_and_login_status() {
        let _serial = serial();
        for script in [
            "exit 1",
            "printf '%s' '{\"result\":\"\"}'",
            "printf '%s' '{\"result\":\"secret detail\",\"is_error\":true}'",
            "printf invalid",
        ] {
            let (_dir, provider) = fixture(ProviderKind::ClaudeCli, script);
            let error = provider
                .complete(&request(), &mut |_| {})
                .unwrap_err()
                .to_string();
            assert!(!error.contains("secret detail"));
        }
        let (_dir, provider) = fixture(
            ProviderKind::CodexCli,
            "[ \"$1\" = login ] && [ \"$2\" = status ]",
        );
        assert!(provider.check_login().is_ok());
    }
    #[test]
    fn timeout_and_running_cancellation_reap_child() {
        let _serial = serial();
        let (_dir, mut provider) = fixture(ProviderKind::ClaudeCli, "exec sleep 30");
        provider.timeout = Duration::from_millis(100);
        assert!(
            provider
                .complete(&request(), &mut |_| {})
                .unwrap_err()
                .to_string()
                .contains("вовремя")
        );
        provider.timeout = Duration::from_secs(30);
        let cancel = provider.cancel.clone();
        let thread = thread::spawn(move || provider.complete(&request(), &mut |_| {}));
        thread::sleep(Duration::from_millis(100));
        cancel.cancel();
        assert!(
            thread
                .join()
                .unwrap()
                .unwrap_err()
                .to_string()
                .contains("отменено")
        );
    }
}

#[cfg(all(test, unix))]
#[test]
fn cli_provider_uses_the_shared_map_reduce_for_long_transcripts() {
    let _serial = tests::serial();
    use std::os::unix::fs::PermissionsExt;
    let dir = tempfile::tempdir().unwrap();
    let binary = dir.path().join("claude");
    fs::write(&binary, "#!/bin/sh\ncat >/dev/null\nprintf x >> \"$(dirname \"$0\")/calls\"\nprintf '%s' '{\"result\":\"brief summary\"}'\n").unwrap();
    fs::set_permissions(&binary, fs::Permissions::from_mode(0o700)).unwrap();
    let config = crate::Config::parse("[defaults]\nlocal='cli'\nhybrid='cli'\n[[providers]]\nid='cli'\nkind='claude-cli'\ncontext_tokens=4096").unwrap().providers.remove(0);
    let provider = CliProvider {
        config,
        binary,
        cancel: Cancellation::default(),
        timeout: Duration::from_secs(30),
    };
    let mut meta = notetaker_core::MeetingMeta::new(
        "Long",
        chrono::DateTime::from_timestamp(1_800_000_000, 0).unwrap(),
    );
    meta.mode = Mode::Hybrid;
    let transcript = notetaker_core::Transcript {
        segments: vec![notetaker_core::Segment {
            id: uuid::Uuid::now_v7(),
            speaker: notetaker_core::SpeakerId("me".into()),
            channel: notetaker_core::Channel::Mic,
            start_ms: 0,
            end_ms: 100,
            text: "Evidence with details. ".repeat(2000),
            source: notetaker_core::Source::Final,
            edited: false,
            lang: None,
        }],
        ..Default::default()
    };
    let text = crate::enhance(
        &meta,
        "notes",
        &transcript,
        &crate::Template::builtin("general").unwrap(),
        &provider,
        &mut |_| {},
    )
    .unwrap();
    assert_eq!(text, "brief summary");
    assert!(fs::read(dir.path().join("calls")).unwrap().len() > 1);
}

#[cfg(test)]
mod login_tests {
    use super::*;

    #[test]
    fn login_launches_the_binary_directly_with_separate_arguments() {
        let path = Path::new(r"C:\Users\A & B\cli\codex.exe");
        for (kind, args) in [
            (ProviderKind::CodexCli, vec!["login"]),
            (ProviderKind::ClaudeCli, vec![]),
        ] {
            let command = login_command(path, kind);
            assert_eq!(command.get_program(), path.as_os_str());
            assert_eq!(command.get_args().collect::<Vec<_>>(), args);
        }
    }
}
