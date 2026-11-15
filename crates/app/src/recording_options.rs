//! Saved sources are exact process/device identities, never a guessed replacement.
use crate::config::Settings;
use notetaker_capture::{Channel, Inventory};
use notetaker_core::app_config::{RecordingOptions, RecordingSource};

#[derive(Debug)]
pub struct SourceUnavailable(pub String);
impl std::fmt::Display for SourceUnavailable {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}
impl std::error::Error for SourceUnavailable {}
fn missing() -> SourceUnavailable {
    SourceUnavailable("Прежний источник недоступен. Выберите источник записи.".into())
}
fn default_device(inventory: &Inventory, channel: Channel) -> Result<&str, SourceUnavailable> {
    inventory
        .devices
        .iter()
        .find(|device| device.channel == channel && device.is_default)
        .map(|device| device.id.as_str())
        .ok_or_else(missing)
}
pub fn remember(
    inventory: &Inventory,
    settings: &Settings,
    process_id: Option<u32>,
    microphone: Option<&str>,
) -> Result<RecordingOptions, SourceUnavailable> {
    let microphone_id = match microphone {
        Some(id) => inventory
            .devices
            .iter()
            .find(|device| device.id == id && device.channel == Channel::Mic)
            .map(|device| device.id.as_str())
            .ok_or_else(missing)?,
        None => default_device(inventory, Channel::Mic)?,
    };
    let source = match process_id {
        Some(pid) => {
            let process = inventory
                .processes
                .iter()
                .find(|process| process.pid == pid && !process.exe.is_empty())
                .ok_or_else(missing)?;
            RecordingSource::Process {
                pid,
                started_at: process.started_at,
                exe: process.exe.clone(),
            }
        }
        None => RecordingSource::System,
    };
    Ok(RecordingOptions {
        model: settings.model.clone(),
        mode: settings.mode,
        source,
        microphone_id: microphone_id.into(),
        output_id: default_device(inventory, Channel::System)?.into(),
    })
}
pub fn resolve(
    options: &RecordingOptions,
    inventory: &Inventory,
) -> Result<Option<u32>, SourceUnavailable> {
    if !inventory
        .devices
        .iter()
        .any(|device| device.channel == Channel::Mic && device.id == options.microphone_id)
        || default_device(inventory, Channel::System)? != options.output_id
    {
        return Err(missing());
    }
    match &options.source {
        RecordingSource::System => Ok(None),
        RecordingSource::Process {
            pid,
            started_at,
            exe,
        } => inventory
            .processes
            .iter()
            .find(|process| {
                process.pid == *pid
                    && *started_at > 0
                    && process.started_at == *started_at
                    && process.exe == *exe
            })
            .map(|process| Some(process.pid))
            .ok_or_else(missing),
    }
}

pub fn apply_default_changes(previous: &Settings, settings: &mut Settings) {
    if let Some(options) = &mut settings.last_recording {
        if previous.model != settings.model {
            options.model = settings.model.clone();
        }
        if previous.mode != settings.mode {
            options.mode = settings.mode;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn options_roundtrip_and_explicit_defaults_update_only_model_and_mode() {
        let inventory = crate::test_ui::recording_inventory();
        let previous = Settings::default();
        let options = remember(&inventory, &previous, Some(42), None).unwrap();
        let mut settings = previous.clone();
        settings.last_recording = Some(options.clone());
        let decoded: Settings =
            toml::from_str(&toml::to_string_pretty(&settings).unwrap()).unwrap();
        assert_eq!(decoded.last_recording.as_ref(), Some(&options));
        settings.model = "auto".into();
        settings.mode = notetaker_core::Mode::Hybrid;
        apply_default_changes(&previous, &mut settings);
        let changed = settings.last_recording.as_ref().unwrap();
        assert_eq!(changed.model, "auto");
        assert_eq!(changed.mode, notetaker_core::Mode::Hybrid);
        assert_eq!(changed.source, options.source);
        assert_eq!(changed.microphone_id, options.microphone_id);
        let stable = settings.clone();
        apply_default_changes(&stable, &mut settings);
        assert_eq!(settings.last_recording, stable.last_recording);
    }
    #[test]
    fn saved_source_requires_same_process_and_devices() {
        let mut inventory = crate::test_ui::recording_inventory();
        let options = remember(&inventory, &Settings::default(), Some(42), None).unwrap();
        assert_eq!(resolve(&options, &inventory).unwrap(), Some(42));
        inventory.processes[0].started_at = 200;
        assert!(resolve(&options, &inventory).is_err());
        inventory.processes[0].started_at = 100;
        inventory.devices[0].id = "other-mic".into();
        assert!(resolve(&options, &inventory).is_err());
    }
}
