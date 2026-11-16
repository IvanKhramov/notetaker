#[path = "../../model_download.rs"]
mod model_download;
use anyhow::{Context, Result, ensure};
use sha2::{Digest, Sha256};
use std::{fs, io::Read, time::Duration};

const ROOT: &str = "https://huggingface.co/Xenova/multilingual-e5-small/resolve/761b726dd34fb83930e26aab4e9ac3899aa1fa78";
const FILES: &[(&str, &str, u64, &str)] = &[
    (
        "onnx/model_quantized.onnx",
        "model.onnx",
        118_308_185,
        "f80102d3f2a1229f387d3c81909990d8945513e347b0eab049f7de3c6f98c193",
    ),
    (
        "tokenizer.json",
        "tokenizer.json",
        17_082_730,
        "0b44a9d7b51c3c62626640cda0e2c2f70fdacdc25bbbd68038369d14ebdf4c39",
    ),
];
pub fn check() -> Result<()> {
    let dir = notetaker_core::embedding::model_dir()?;
    check_directory(&dir)
}
fn check_directory(dir: &std::path::Path) -> Result<()> {
    for (_, name, size, _) in FILES {
        ensure!(
            fs::metadata(dir.join(name)).map(|m| m.len()).unwrap_or(0) == *size,
            "Модель отсутствует или повреждена: notetaker-embed download"
        );
    }
    Ok(())
}

pub fn download_plan() -> Result<()> {
    let ready = check().is_ok();
    for (_, name, size, _) in FILES {
        println!("NOTETAKER_DOWNLOAD_PLAN {name} {size} {}", u8::from(ready));
    }
    Ok(())
}
pub fn download() -> Result<()> {
    let root = notetaker_core::embedding::models_dir()?;
    fs::create_dir_all(&root)?;
    let _lock = model_download::lock(&root, notetaker_core::embedding::MODEL_DIRECTORY)?;
    let staging = model_download::staging(&root, notetaker_core::embedding::MODEL_DIRECTORY)?;
    if check().is_ok() {
        fs::remove_dir_all(staging)?;
        return Ok(());
    }
    for (source, name, size, sha) in FILES {
        let partial = root.join(format!(".e5-{name}.part"));
        model_download::download(
            &format!("{ROOT}/{source}"),
            &partial,
            *size,
            name,
            Duration::from_secs(60),
        )?;
        let mut file = fs::File::open(&partial)?;
        let mut hash = Sha256::new();
        let mut bytes = 0u64;
        let mut buffer = [0u8; 65536];
        loop {
            let n = file.read(&mut buffer)?;
            if n == 0 {
                break;
            }
            bytes += n as u64;
            hash.update(&buffer[..n]);
        }
        if bytes != *size || format!("{:x}", hash.finalize()) != *sha {
            drop(file);
            model_download::discard_corrupt(&partial)?;
            anyhow::bail!("Не совпал SHA-256 или размер {name}");
        }
        drop(file);
        fs::copy(&partial, staging.join(name))?;
        fs::OpenOptions::new()
            .write(true)
            .open(staging.join(name))?
            .sync_all()?;
        eprintln!("NOTETAKER_MODEL_INSTALLED {name}");
    }
    let dest = notetaker_core::embedding::model_dir()?;
    fs::create_dir_all(&dest)?;
    for (_, name, _, _) in FILES {
        fs::rename(staging.join(name), dest.join(name)).context("Не удалось установить модель")?;
    }
    fs::remove_dir_all(staging)?;
    for (_, name, _, _) in FILES {
        fs::remove_file(root.join(format!(".e5-{name}.part")))?;
    }
    eprintln!("Поиск по смыслу: модель установлена");
    Ok(())
}

#[cfg(test)]
mod legacy_install_tests {
    use super::*;
    #[test]
    fn released_embedding_model_needs_no_markers_and_rejects_each_bad_file() {
        let root = tempfile::tempdir().unwrap();
        for (_, name, size, _) in FILES {
            fs::File::create(root.path().join(name))
                .unwrap()
                .set_len(*size)
                .unwrap();
        }
        assert!(check_directory(root.path()).is_ok());
        assert_eq!(fs::read_dir(root.path()).unwrap().count(), FILES.len());
        for (_, name, size, _) in FILES {
            fs::OpenOptions::new()
                .write(true)
                .open(root.path().join(name))
                .unwrap()
                .set_len(size - 1)
                .unwrap();
            assert!(check_directory(root.path()).is_err());
            fs::remove_file(root.path().join(name)).unwrap();
            assert!(check_directory(root.path()).is_err());
            fs::File::create(root.path().join(name))
                .unwrap()
                .set_len(*size)
                .unwrap();
            assert!(check_directory(root.path()).is_ok());
        }
    }
}
