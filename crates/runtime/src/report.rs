use anyhow::{Context, Result};
use std::{
    fs::{self, File},
    io::Write,
    path::{Path, PathBuf},
    time::{SystemTime, UNIX_EPOCH},
};
use zip::{ZipWriter, write::SimpleFileOptions};

/// An allowlist, not a denylist: future config fields remain private by default.
pub fn public_config(config: &toml::Value) -> String {
    let mut safe = toml::map::Map::new();
    for key in [
        "detect_calls",
        "detect_browsers",
        "detect_any_microphone",
        "auto_summary",
        "diarize",
        "voice_library",
        "mcp_exclude_local",
        "check_updates",
    ] {
        if let Some(value) = config.get(key).and_then(toml::Value::as_bool) {
            safe.insert(key.into(), value.into());
        }
    }
    for key in ["retention_days", "auto_stop_minutes", "speakers"] {
        if let Some(value) = config.get(key).and_then(toml::Value::as_integer) {
            safe.insert(key.into(), value.into());
        }
    }
    for (key, allowed) in [
        ("model", &["gigaam-v3", "parakeet-v3"][..]),
        ("mode", &["local", "hybrid"][..]),
        ("theme", &["system", "dark", "light"][..]),
    ] {
        if let Some(value) = config
            .get(key)
            .and_then(toml::Value::as_str)
            .filter(|v| allowed.contains(v))
        {
            safe.insert(key.into(), value.into());
        }
    }
    toml::to_string_pretty(&toml::Value::Table(safe)).unwrap_or_default()
}

pub fn collect(config: &toml::Value, destination: &Path) -> Result<PathBuf> {
    fs::create_dir_all(destination)?;
    let stamp = SystemTime::now().duration_since(UNIX_EPOCH)?.as_nanos();
    let path = destination.join(format!("notetaker-report-{stamp}.zip"));
    let file = File::create_new(&path)?;
    let mut zip = ZipWriter::new(file);
    let options = SimpleFileOptions::default().compression_method(zip::CompressionMethod::Deflated);
    zip.start_file("versions.txt", options)?;
    writeln!(
        zip,
        "notetaker={}\nos={}\narch={}\n",
        env!("CARGO_PKG_VERSION"),
        std::env::consts::OS,
        std::env::consts::ARCH
    )?;
    zip.start_file("config-redacted.toml", options)?;
    zip.write_all(public_config(config).as_bytes())?;
    if let Some(logs) = crate::diagnostics::directory().filter(|p| p.is_dir()) {
        for entry in fs::read_dir(logs)? {
            let entry = entry?;
            if crate::diagnostics::is_log(&entry.path()) && entry.file_type()?.is_file() {
                zip.start_file(
                    format!("logs/{}", entry.file_name().to_string_lossy()),
                    options,
                )?;
                // Bound report size even if a local file was externally modified.
                let mut file = std::io::Read::take(File::open(entry.path())?, 2 * 1024 * 1024);
                std::io::copy(&mut file, &mut zip)?;
            }
        }
    }
    zip.finish().context("Не удалось завершить отчёт")?;
    Ok(path)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn report_only_includes_typed_public_fields() {
        let config: toml::Value = toml::from_str("check_updates = true\nmodel = 'gigaam-v3'\nvocabulary = 'private notes'\nlibrary = '/private'\napi_key = 'secret'\nbase_url = 'https://secret'\ncalendars = [{url = 'https://secret.ics', name = 'private'}]\n[future]\nkey = 'secret'\n").unwrap();
        let safe = public_config(&config);
        assert!(safe.contains("check_updates = true"));
        assert!(safe.contains("gigaam-v3"));
        for value in [
            "private",
            "secret",
            "calendars",
            "api_key",
            "base_url",
            "future",
        ] {
            assert!(!safe.contains(value));
        }
    }
}
