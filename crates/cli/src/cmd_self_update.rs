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
        let result = std::process::Command::new(&args.installer)
            .args([
                "/VERYSILENT",
                "/SUPPRESSMSGBOXES",
                "/NORESTART",
                "/CLOSEAPPLICATIONS=no",
            ])
            .arg(if notetaker_runtime::windows::autostart() {
                "/MERGETASKS=autostart"
            } else {
                "/MERGETASKS=!autostart"
            })
            .arg(format!("/DIR={}", args.install.display()))
            .status();
        let success = result.is_ok_and(|status| status.success());
        notetaker_runtime::diagnostics::event(if success {
            "installer update completed"
        } else {
            "installer update failed"
        });
        if !success && let Some(logs) = notetaker_runtime::diagnostics::directory() {
            let _ = std::fs::write(logs.join("update-failed.flag"), b"failed");
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
    #[cfg(not(windows))]
    {
        let _ = args.parent_pid;
        anyhow::bail!("Самообновление доступно только в Windows");
    }
}
