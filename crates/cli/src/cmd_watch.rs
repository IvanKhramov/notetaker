use anyhow::Result;
use clap::Args as ClapArgs;

#[derive(ClapArgs)]
pub struct Args {
    /// Include browser microphone use (may also be dictation or voice messages).
    #[arg(long)]
    browsers: bool,
    /// Additional executable names to treat as calling applications.
    #[arg(long)]
    app: Vec<String>,
    #[arg(long, default_value_t = 5)]
    start_seconds: u64,
    #[arg(long, default_value_t = 20)]
    end_seconds: u64,
    #[arg(long, default_value_t = 10)]
    dismiss_minutes: u64,
}

#[cfg(any(windows, target_os = "macos"))]
pub fn run(args: Args) -> Result<()> {
    use anyhow::Context;
    use notetaker_detect::{AppRule, CallDetector, DetectConfig, DetectEvent};
    use std::{
        sync::{
            Arc,
            atomic::{AtomicBool, Ordering},
        },
        thread,
        time::{Duration, Instant},
    };

    let mut config = DetectConfig {
        detect_browsers: args.browsers,
        start_delay: Duration::from_secs(args.start_seconds),
        end_delay: Duration::from_secs(args.end_seconds),
        dismiss_cooldown: Duration::from_secs(
            args.dismiss_minutes
                .checked_mul(60)
                .context("dismiss cooldown too large")?,
        ),
        ..DetectConfig::default()
    };
    config.apps.extend(args.app.into_iter().map(|exe| AppRule {
        name: exe.clone(),
        exe,
        browser: false,
    }));
    #[cfg(windows)]
    let provider = notetaker_detect::WindowsSnapshot;
    #[cfg(target_os = "macos")]
    let provider = notetaker_detect::MacosSnapshot;
    let mut detector = CallDetector::new(provider, config);
    let stop = Arc::new(AtomicBool::new(false));
    let signal = Arc::clone(&stop);
    ctrlc::set_handler(move || signal.store(true, Ordering::Release))?;
    let origin = Instant::now();
    eprintln!(
        "Watching for calls (Ctrl+C to stop). Recording requires confirmation in the application."
    );
    while !stop.load(Ordering::Acquire) {
        for event in detector.poll(origin.elapsed()) {
            println!("[{:.1}s] {event:?}", origin.elapsed().as_secs_f64());
            if let DetectEvent::CallStarted { app, .. } = event {
                println!("Похоже, идёт звонок в {app}. Записать?");
            }
        }
        for warning in detector.take_warnings() {
            eprintln!("Warning: {warning}");
        }
        thread::sleep(Duration::from_secs(1));
    }
    Ok(())
}

#[cfg(not(any(windows, target_os = "macos")))]
pub fn run(_: Args) -> Result<()> {
    anyhow::bail!("call detection not supported on this OS yet")
}
