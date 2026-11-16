//! All config.toml writers share ownership rules and an inter-process lock.
use anyhow::{Context, Result};
use std::{fs, io::Write, path::Path, time::Duration};

pub enum Section {
    Ai,
    Ktalk,
}
pub fn save(path: &Path, section: Section, value: toml::Value) -> Result<()> {
    let parent = path.parent().context("Нет каталога конфигурации")?;
    fs::create_dir_all(parent)?;
    let lock_path = path.with_extension("toml.lock");
    let _lock = notetaker_runtime::file_lock::exclusive(&lock_path, Duration::from_secs(5))
        .with_context(|| format!("Не удалось заблокировать {}", path.display()))?;
    let mut root: toml::Table = match fs::read_to_string(path) {
        Ok(text) => toml::from_str(&text).map_err(|_| {
            anyhow::anyhow!("Некорректный TOML в {}; файл не изменён", path.display())
        })?,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Default::default(),
        Err(error) => return Err(error).with_context(|| format!("Чтение {}", path.display())),
    };
    match section {
        Section::Ktalk => {
            root.insert("ktalk".into(), value);
        }
        Section::Ai => {
            root.retain(|key, _| key == "ktalk");
            let tables = value
                .as_table()
                .context("Настройки AI должны быть таблицей")?;
            root.extend(
                tables
                    .iter()
                    .filter(|(key, _)| key.as_str() != "ktalk")
                    .map(|(key, value)| (key.clone(), value.clone())),
            );
        }
    }
    let mut file = tempfile::NamedTempFile::new_in(parent)?;
    file.write_all(toml::to_string_pretty(&root)?.as_bytes())?;
    file.as_file().sync_all()?;
    file.persist(path).map_err(|error| error.error)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn ai_and_tolk_writers_preserve_each_other_and_reject_unparsable_files() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("config.toml");
        fs::write(&path,"[defaults]\nlocal=123\n[ktalk]\nbase_url='https://neutral.ktalk.ru'\ncredential_id='test'\nfuture={a=[1,2]}\n").unwrap();
        let old: toml::Value = toml::from_str(&fs::read_to_string(&path).unwrap()).unwrap();
        save(
            &path,
            Section::Ai,
            toml::from_str("[defaults]\nlocal='local'\nhybrid='local'\nproviders=[]\n").unwrap(),
        )
        .unwrap();
        let current: toml::Value = toml::from_str(&fs::read_to_string(&path).unwrap()).unwrap();
        assert_eq!(current["ktalk"], old["ktalk"]);
        let barrier = std::sync::Arc::new(std::sync::Barrier::new(3));
        let mut threads = Vec::new();
        for ai in [false, true] {
            let path = path.clone();
            let barrier = barrier.clone();
            threads.push(std::thread::spawn(move || {
                barrier.wait();
                save(
                    &path,
                    if ai { Section::Ai } else { Section::Ktalk },
                    toml::from_str(if ai {
                        "[defaults]\nlocal='updated'"
                    } else {
                        "base_url='https://new.ktalk.ru'\ncredential_id='new'"
                    })
                    .unwrap(),
                )
                .unwrap();
            }));
        }
        barrier.wait();
        for thread in threads {
            thread.join().unwrap();
        }
        let value: toml::Value = toml::from_str(&fs::read_to_string(&path).unwrap()).unwrap();
        assert_eq!(value["defaults"]["local"].as_str(), Some("updated"));
        assert_eq!(value["ktalk"]["credential_id"].as_str(), Some("new"));
        fs::write(&path, "[ktalk\n").unwrap();
        let before = fs::read(&path).unwrap();
        let error = save(&path, Section::Ai, toml::Value::Table(Default::default())).unwrap_err();
        assert!(error.to_string().contains("config.toml"));
        assert_eq!(fs::read(path).unwrap(), before);
    }
}
