use anyhow::{Context, Result, ensure};
use notetaker_asr_proto::TimeRange;
use notetaker_core::Library;
use std::{
    fs::{self, File},
    path::{Path, PathBuf},
};

#[derive(Debug)]
pub enum LearningSpaceError {
    Insufficient,
    Unavailable,
    Busy,
}
impl std::fmt::Display for LearningSpaceError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(match self {
            Self::Insufficient => "Недостаточно места для обучения голосов. Освободите место на диске и повторите обучение.",
            Self::Unavailable => "Не удалось подготовить временные файлы обучения. Проверьте свободное место и доступ к каталогу данных.",
            Self::Busy => "Временные файлы обучения используются другим процессом. Остановите его и повторите обучение.",
        })
    }
}
impl std::error::Error for LearningSpaceError {}

fn paths(library: &Library) -> Result<(File, PathBuf)> {
    let private = library.private_state_dir();
    let root = private
        .parent()
        .and_then(Path::parent)
        .context("Нет каталога локальных данных")?;
    fs::create_dir_all(root)?;
    let lock = File::options()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .open(root.join("tolk-learning-temp.lock"))?;
    Ok((lock, root.join("tolk-learning-temp")))
}

/// The sibling lock stays open across cleanup and processing, including on Windows.
pub fn cleanup_learning_temp(library: &Library) -> Result<()> {
    let (lock, path) = paths(library)?;
    match lock.try_lock() {
        Ok(()) => {}
        Err(std::fs::TryLockError::WouldBlock) => return Ok(()), // Active work is not an orphan.
        Err(std::fs::TryLockError::Error(error)) => return Err(error.into()),
    }
    if path.try_exists()? {
        fs::remove_dir_all(path)?;
    }
    Ok(())
}

pub(super) fn workspace(
    library: &Library,
    duration_secs: u32,
) -> Result<(File, tempfile::TempDir)> {
    let (lock, root) = paths(library).map_err(|_| LearningSpaceError::Unavailable)?;
    lock.try_lock().map_err(|error| match error {
        std::fs::TryLockError::WouldBlock => anyhow::Error::from(LearningSpaceError::Busy),
        std::fs::TryLockError::Error(_) => anyhow::Error::from(LearningSpaceError::Unavailable),
    })?;
    // Once the exclusive lock is acquired, any remaining work belongs to an exited process.
    if root
        .try_exists()
        .map_err(|_| LearningSpaceError::Unavailable)?
    {
        fs::remove_dir_all(&root).map_err(|_| LearningSpaceError::Unavailable)?;
    }
    // Download estimate, simultaneous decoded mono f32 PCM and the recording reserve.
    let duration = u64::from(duration_secs);
    let required = (duration * 15_000).min(300_000_000)
        + duration.min(12 * 60 * 60) * 64_000
        + notetaker_runtime::recording::MIN_FREE_BYTES;
    let free = notetaker_runtime::recording::available_space(
        root.parent().context("Нет каталога локальных данных")?,
    )
    .map_err(|_| LearningSpaceError::Unavailable)?;
    check_space(required, free)?;
    fs::create_dir_all(&root).map_err(|_| LearningSpaceError::Unavailable)?;
    let work = tempfile::Builder::new()
        .prefix("recording-")
        .tempdir_in(root)
        .map_err(|_| LearningSpaceError::Unavailable)?;
    Ok((lock, work))
}

fn check_space(required: u64, free: u64) -> Result<()> {
    ensure!(free >= required, LearningSpaceError::Insufficient);
    Ok(())
}

/// Stream only the selected fragments; the worker never receives the full meeting WAV.
pub(super) fn fragments(
    input: &Path,
    output: &Path,
    ranges: Vec<TimeRange>,
    cancelled: &impl Fn() -> bool,
) -> Result<Vec<TimeRange>> {
    let mut reader = hound::WavReader::open(input)?;
    let spec = reader.spec();
    ensure!(
        spec.channels == 1
            && spec.sample_rate == 16000
            && spec.sample_format == hound::SampleFormat::Float
            && spec.bits_per_sample == 32,
        "Некорректный PCM для обучения голосов"
    );
    let mut writer = hound::WavWriter::create(output, spec)?;
    let mut result = Vec::new();
    let mut count = 0u64;
    for range in ranges.into_iter().take(8) {
        ensure!(!cancelled(), "Обучение голосов отменено");
        let start = range
            .start_ms
            .saturating_mul(16)
            .min(u64::from(reader.duration()));
        let end = range
            .end_ms
            .saturating_mul(16)
            .min(start.saturating_add(15 * 16000))
            .min(u64::from(reader.duration()));
        ensure!(end >= start, "Некорректный интервал обучения голосов");
        reader.seek(u32::try_from(start)?)?;
        let begin = count;
        for (i, sample) in reader
            .samples::<f32>()
            .take((end - start) as usize)
            .enumerate()
        {
            if i % 16000 == 0 {
                ensure!(!cancelled(), "Обучение голосов отменено");
            }
            writer.write_sample(sample?)?;
            count += 1;
        }
        result.push(TimeRange {
            start_ms: begin / 16,
            end_ms: count / 16,
        });
    }
    writer.finalize()?;
    Ok(result)
}

#[cfg(test)]
pub(super) static TEST_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn disk_guard_and_crash_cleanup_respect_the_active_lock() {
        let _test = TEST_LOCK.lock().unwrap();
        assert!(check_space(100, 99).unwrap_err().is::<LearningSpaceError>());
        assert!(check_space(100, 100).is_ok());
        let dir = tempfile::tempdir().unwrap();
        let lib = Library::open(dir.path()).unwrap();
        let (lock, temp) = workspace(&lib, 12).unwrap();
        let orphan = temp.keep();
        fs::write(orphan.join("private-audio.mp4"), b"synthetic").unwrap();
        cleanup_learning_temp(&lib).unwrap();
        assert!(orphan.exists());
        drop(lock);
        cleanup_learning_temp(&lib).unwrap();
        assert!(!orphan.exists());
        cleanup_learning_temp(&lib).unwrap();
    }
    #[test]
    fn a_three_hour_source_produces_at_most_120_seconds_for_embed() {
        use std::io::{Seek, Write};
        let dir = tempfile::tempdir().unwrap();
        let input = dir.path().join("long.wav");
        let output = dir.path().join("fragments.wav");
        let bytes = 3u32 * 60 * 60 * 16000 * 4;
        let mut file = File::create(&input).unwrap();
        file.write_all(b"RIFF").unwrap();
        file.write_all(&(bytes + 36).to_le_bytes()).unwrap();
        file.write_all(b"WAVEfmt ").unwrap();
        file.write_all(&16u32.to_le_bytes()).unwrap();
        file.write_all(&3u16.to_le_bytes()).unwrap();
        file.write_all(&1u16.to_le_bytes()).unwrap();
        file.write_all(&16000u32.to_le_bytes()).unwrap();
        file.write_all(&64000u32.to_le_bytes()).unwrap();
        file.write_all(&4u16.to_le_bytes()).unwrap();
        file.write_all(&32u16.to_le_bytes()).unwrap();
        file.write_all(b"data").unwrap();
        file.write_all(&bytes.to_le_bytes()).unwrap();
        file.set_len(44 + u64::from(bytes)).unwrap();
        file.seek(std::io::SeekFrom::Start(44 + 3600 * 16000 * 4))
            .unwrap();
        file.write_all(&0.5f32.to_le_bytes()).unwrap();
        drop(file);
        let ranges = (0..8)
            .map(|n| TimeRange {
                start_ms: 3_600_000 + n * 20_000,
                end_ms: 3_615_000 + n * 20_000,
            })
            .collect();
        let result = fragments(&input, &output, ranges, &|| false).unwrap();
        assert_eq!(result.len(), 8);
        assert_eq!(result[0].start_ms, 0);
        assert_eq!(result[7].end_ms, 120_000);
        assert!(fs::metadata(output.clone()).unwrap().len() < 8_000_000);
        let mut reader = hound::WavReader::open(output).unwrap();
        assert_eq!(reader.samples::<f32>().next().unwrap().unwrap(), 0.5);
    }
}
