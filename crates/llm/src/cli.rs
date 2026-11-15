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
    #[cfg(target_os = "macos")]
    let path = Some(macos_cli_path());
    #[cfg(not(target_os = "macos"))]
    let path = std::env::var_os("PATH");
    find_in_path(kind, path.as_deref())
}

#[cfg(target_os = "macos")]
fn macos_cli_path() -> std::ffi::OsString {
    // Finder does not inherit the user's interactive shell PATH.
    let mut dirs: Vec<_> = std::env::var_os("PATH")
        .as_deref()
        .map(std::env::split_paths)
        .into_iter()
        .flatten()
        .collect();
    if let Some(home) = directories::UserDirs::new() {
        dirs.extend([
            home.home_dir().join(".local/bin"),
            home.home_dir().join(".npm-global/bin"),
            home.home_dir().join(".cargo/bin"),
        ]);
    }
    dirs.extend(["/opt/homebrew/bin", "/usr/local/bin", "/usr/bin", "/bin"].map(PathBuf::from));
    std::env::join_paths(dirs).unwrap_or_default()
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
        // The standalone Codex installer puts the launcher here.
        if let Some(local) = std::env::var_os("LOCALAPPDATA") {
            dirs.push(PathBuf::from(local).join(r"Programs\OpenAI\Codex\bin"));
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
    #[cfg(target_os = "macos")]
    command.env("PATH", macos_cli_path());
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
    let error_path = dir.join("stderr.txt");
    fs::write(&input_path, prompt)?;
    let working_dir = dir.join("work");
    fs::create_dir(&working_dir)?;
    command
        .current_dir(working_dir)
        .stdin(File::open(input_path)?)
        .stdout(File::create(&output_path)?)
        .stderr(File::create(&error_path)?);
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
    if !status.success() {
        // The CLI's own words name the cause: no sign-in, a missing
        // prerequisite, an unknown model. The answer goes to stdout when the
        // CLI reports in JSON, so look there when stderr is empty.
        let said = [&error_path, &output_path]
            .into_iter()
            .map(|path| failure_tail(&file_tail(path)))
            .find(|text| !text.is_empty())
            .unwrap_or_default();
        anyhow::bail!(
            "CLI завершился с ошибкой. Проверьте вход, модель и лимиты подписки.{}",
            if said.is_empty() {
                String::new()
            } else {
                format!(" Ответ CLI: {said}")
            }
        );
    }
    ensure!(
        fs::metadata(&output_path)?.len() <= 16 * 1024 * 1024,
        "Слишком большой ответ CLI"
    );
    fs::read_to_string(output_path).context("Некорректный вывод CLI")
}

/// The end of a CLI log, read without loading a large file.
fn file_tail(path: &Path) -> String {
    use std::io::{Read, Seek, SeekFrom};
    let mut bytes = Vec::new();
    if let Ok(mut file) = File::open(path) {
        let length = file.metadata().map(|meta| meta.len()).unwrap_or(0);
        let _ = file.seek(SeekFrom::Start(length.saturating_sub(8 * 1024)));
        let _ = file.take(8 * 1024).read_to_end(&mut bytes);
    }
    String::from_utf8_lossy(&bytes).into_owned()
}

/// The last lines of what a failed CLI printed, short enough for a settings card.
fn failure_tail(output: &str) -> String {
    let lines: Vec<&str> = output
        .lines()
        .map(str::trim)
        .filter(|line| !line.is_empty())
        .collect();
    let tail = lines[lines.len().saturating_sub(3)..].join(" ");
    tail.chars().take(300).collect()
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
    fn is_cancelled(&self) -> bool {
        self.cancel.is_cancelled()
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

/// The sign-in arguments of each CLI. Both open the browser themselves.
fn login_args(kind: ProviderKind) -> &'static [&'static str] {
    if kind == ProviderKind::CodexCli {
        &["login"]
    } else {
        &["auth", "login"]
    }
}

/// The vendor's own installer script. Both install into the user's profile and
/// need no administrator rights.
fn installer_url(kind: ProviderKind) -> &'static str {
    match (kind == ProviderKind::ClaudeCli, cfg!(windows)) {
        (true, true) => "https://claude.ai/install.ps1",
        (true, false) => "https://claude.ai/install.sh",
        (false, true) => "https://chatgpt.com/codex/install.ps1",
        (false, false) => "https://chatgpt.com/codex/install.sh",
    }
}

fn install_command(kind: ProviderKind) -> Command {
    let url = installer_url(kind);
    #[cfg(windows)]
    let mut command = {
        use std::os::windows::process::CommandExt;
        let mut command = Command::new("powershell.exe");
        command
            .args([
                "-NoProfile",
                "-NonInteractive",
                "-ExecutionPolicy",
                "Bypass",
                "-Command",
            ])
            .arg(format!("irm {url} | iex"))
            .creation_flags(0x08000000); // CREATE_NO_WINDOW
        command
    };
    #[cfg(not(windows))]
    let mut command = {
        // Download first: a failed download must not run a partial script.
        let mut command = Command::new("/bin/sh");
        command
            .arg("-c")
            .arg(r#"set -e; script=$(mktemp); trap 'rm -f "$script"' EXIT; curl -fsSL "$1" -o "$script"; "$2" "$script""#)
            .arg("sh")
            .arg(url)
            .arg(if kind == ProviderKind::ClaudeCli { "bash" } else { "sh" });
        command
    };
    // The Codex installer asks questions unless told not to.
    command.env("CODEX_NON_INTERACTIVE", "1");
    command
}

/// Runs an installer without a terminal and returns the end of its output on failure.
fn run_installer(mut command: Command, timeout: Duration) -> Result<()> {
    let output = tempfile::tempfile().context("Не удалось создать временный файл")?;
    let mut child = command
        .stdin(Stdio::null())
        .stdout(output.try_clone()?)
        .stderr(output.try_clone()?)
        .spawn()
        .context("Не удалось запустить установщик")?;
    let started = Instant::now();
    let status = loop {
        if let Some(status) = child.try_wait()? {
            break status;
        }
        if started.elapsed() > timeout {
            let _ = child.kill();
            let _ = child.wait();
            anyhow::bail!("Установка не завершилась за {} мин", timeout.as_secs() / 60);
        }
        thread::sleep(Duration::from_millis(200));
    };
    if status.success() {
        return Ok(());
    }
    use std::io::{Read, Seek};
    let mut output = output;
    let mut text = Vec::new();
    output.rewind()?;
    output.read_to_end(&mut text)?;
    let text = String::from_utf8_lossy(&text);
    let tail: Vec<_> = text
        .lines()
        .filter(|line| !line.trim().is_empty())
        .rev()
        .take(4)
        .collect();
    let tail: Vec<_> = tail.into_iter().rev().collect();
    anyhow::bail!("Установщик завершился с ошибкой. {}", tail.join(" "))
}

/// Installs the CLI with its vendor's installer and confirms it can be found.
pub fn install_cli(kind: ProviderKind) -> Result<()> {
    run_installer(install_command(kind), Duration::from_secs(600))?;
    ensure!(
        find_cli(kind).is_some(),
        "Установка завершилась, но программа не найдена. Перезапустите notetaker и нажмите «Проверить снова»."
    );
    Ok(())
}

/// Signs in through the CLI's own command. No console window: the CLI opens
/// the browser and waits for it, and the caller shows the wait and can cancel.
pub fn login(kind: ProviderKind, cancel: &Cancellation) -> Result<()> {
    let binary = find_cli(kind).context("CLI не установлен")?;
    let mut command = background(&binary);
    command.args(login_args(kind));
    wait_login(command, Duration::from_secs(300), cancel)
}

fn wait_login(mut command: Command, timeout: Duration, cancel: &Cancellation) -> Result<()> {
    let dir = tempfile::tempdir()?;
    let output_path = dir.path().join("login.txt");
    let output = File::create(&output_path)?;
    command.stdout(output.try_clone()?).stderr(output);
    #[cfg(unix)]
    {
        use std::os::unix::process::CommandExt;
        command.process_group(0);
    }
    let mut child = command
        .spawn()
        .context("Не удалось запустить вход; проверьте установку CLI")?;
    let started = Instant::now();
    let status = loop {
        if cancel.cancelled() {
            terminate(&mut child);
            anyhow::bail!("Вход отменён");
        }
        if started.elapsed() >= timeout {
            terminate(&mut child);
            anyhow::bail!("Вход не завершён за 5 минут. Повторите попытку");
        }
        match child.try_wait() {
            Ok(Some(status)) => break status,
            Ok(None) => {}
            Err(error) => {
                terminate(&mut child);
                return Err(error).context("Не удалось дождаться входа");
            }
        }
        thread::sleep(Duration::from_millis(100));
    };
    if !status.success() {
        let said = failure_tail(&file_tail(&output_path));
        anyhow::bail!(
            "Вход не выполнен.{}",
            if said.is_empty() {
                String::new()
            } else {
                format!(" Ответ CLI: {said}")
            }
        );
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
    #[cfg(unix)]
    fn installer_runs_the_vendor_script_and_reports_its_last_lines() {
        for (kind, url, shell) in [
            (
                ProviderKind::ClaudeCli,
                "https://claude.ai/install.sh",
                "bash",
            ),
            (
                ProviderKind::CodexCli,
                "https://chatgpt.com/codex/install.sh",
                "sh",
            ),
        ] {
            let command = install_command(kind);
            let args: Vec<_> = command.get_args().collect();
            assert_eq!(command.get_program(), "/bin/sh");
            assert_eq!(&args[2..], ["sh", url, shell]);
        }
        let mut ok = Command::new("/bin/sh");
        ok.args(["-c", "echo done"]);
        run_installer(ok, Duration::from_secs(10)).unwrap();
        let mut failing = Command::new("/bin/sh");
        failing.args(["-c", "echo first; echo; echo 'network is down' >&2; exit 3"]);
        let error = run_installer(failing, Duration::from_secs(10))
            .unwrap_err()
            .to_string();
        assert!(error.contains("first network is down"), "{error}");
        let mut slow = Command::new("/bin/sh");
        slow.args(["-c", "sleep 30"]);
        assert!(run_installer(slow, Duration::from_millis(300)).is_err());
    }

    #[test]
    fn failure_tail_keeps_the_last_lines_and_is_bounded() {
        assert_eq!(failure_tail(""), "");
        assert_eq!(
            failure_tail("one\n\n two \nthree\nfour\n"),
            "two three four"
        );
        assert_eq!(failure_tail(&"я".repeat(1000)).chars().count(), 300);
    }
    #[test]
    fn login_arguments_are_the_sign_in_command_of_each_cli() {
        assert_eq!(login_args(ProviderKind::CodexCli), ["login"]);
        assert_eq!(login_args(ProviderKind::ClaudeCli), ["auth", "login"]);
    }
    #[cfg(unix)]
    #[test]
    fn login_wait_reports_the_cli_error_and_stops_on_cancel_and_timeout() {
        let shell = |script: &str| {
            let mut command = background(Path::new("/bin/sh"));
            command.args(["-c", script]);
            command
        };
        let long = Duration::from_secs(30);
        assert!(wait_login(shell("exit 0"), long, &Cancellation::default()).is_ok());
        let error = wait_login(
            shell("echo first; echo browser is missing >&2; exit 3"),
            long,
            &Cancellation::default(),
        )
        .unwrap_err()
        .to_string();
        assert!(error.contains("browser is missing"), "{error}");
        let cancel = Cancellation::default();
        cancel.cancel();
        let error = wait_login(shell("sleep 30"), long, &cancel).unwrap_err();
        assert_eq!(error.to_string(), "Вход отменён");
        let error = wait_login(
            shell("sleep 30"),
            Duration::from_millis(200),
            &Cancellation::default(),
        )
        .unwrap_err();
        assert!(error.to_string().contains("5 минут"));
    }
}
