use anyhow::{Result, ensure};
use std::path::PathBuf;

#[derive(clap::Args)]
pub struct Args {
    #[arg(long)]
    parent_pid: u32,
    #[arg(long)]
    install: PathBuf,
    #[arg(long)]
    installer: PathBuf,
    #[arg(long)]
    ready_file: PathBuf,
}

pub fn run(args: Args) -> Result<()> {
    let _ = notetaker_runtime::diagnostics::init();
    ensure!(
        args.install.is_absolute() && args.installer.is_absolute() && args.ready_file.is_absolute(),
        "Требуются абсолютные пути"
    );
    ensure!(
        args.install.join("notetaker-app.exe").is_file() && args.installer.is_file(),
        "Нет приложения или установщика"
    );
    #[cfg(windows)]
    {
        std::fs::write(&args.ready_file, b"ready")?;
        notetaker_runtime::windows::wait_for_exit(
            args.parent_pid,
            std::time::Duration::from_secs(120),
        )?;
        notetaker_runtime::windows::Instance::wait_for_exit(std::time::Duration::from_secs(30))?;
        let logs = notetaker_runtime::diagnostics::directory();
        let mut installer = std::process::Command::new(&args.installer);
        installer
            .args([
                "/VERYSILENT",
                "/SUPPRESSMSGBOXES",
                "/NORESTART",
                "/NOCLOSEAPPLICATIONS",
            ])
            .arg(if notetaker_runtime::windows::autostart() {
                "/MERGETASKS=autostart"
            } else {
                "/MERGETASKS=!autostart"
            })
            .arg(format!("/DIR={}", args.install.display()));
        if let Some(logs) = &logs {
            installer.arg(format!(
                "/LOG={}",
                logs.join("installer-update.log").display()
            ));
        }
        let (success, outcome) = match installer.status() {
            Ok(status) => (
                status.success(),
                status
                    .code()
                    .map_or_else(|| "code=unknown".into(), |code| format!("code={code}")),
            ),
            Err(error) => (false, format!("spawn_error={error}")),
        };
        let event = if success { "completed" } else { "failed" };
        notetaker_runtime::diagnostics::event(&format!("installer update {event} {outcome}"));
        if !success && let Some(logs) = logs {
            let _ = std::fs::write(logs.join("update-failed.flag"), &outcome);
        }
        // Inno owns file replacement. [Run] skips silent installations, so one
        // relaunch here also recovers the previous build on installer failure.
        if let Some(directory) = args.installer.parent() {
            let _ = std::fs::write(directory.join("finished"), b"finished");
        }
        std::process::Command::new(args.install.join("notetaker-app.exe"))
            .current_dir(&args.install)
            .spawn()?;
        ensure!(
            success,
            "Установщик не завершил обновление. Повторите установку вручную"
        );
        Ok(())
    }
    #[cfg(target_os = "macos")]
    {
        let _ = args;
        notetaker_runtime::unix::open_url(&format!(
            "{}/releases/latest",
            notetaker_runtime::update::REPOSITORY
        ))?;
        println!(
            "Скачайте DMG, завершите запись, закройте notetaker и замените приложение в Applications."
        );
        Ok(())
    }
    #[cfg(not(any(windows, target_os = "macos")))]
    {
        let _ = args.parent_pid;
        anyhow::bail!("Самообновление доступно только в Windows");
    }
}
