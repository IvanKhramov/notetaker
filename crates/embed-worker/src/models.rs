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
    let staging = tempfile::tempdir_in(&root)?;
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
            model_download::discard_corrupt(&partial)?;
            anyhow::bail!("Не совпал SHA-256 или размер {name}");
        }
        fs::copy(&partial, staging.path().join(name))?;
        fs::OpenOptions::new()
            .write(true)
            .open(staging.path().join(name))?
            .sync_all()?;
        eprintln!("NOTETAKER_MODEL_INSTALLED {name}");
    }
    let dest = notetaker_core::embedding::model_dir()?;
    if dest.exists() {
        // Replace individual verified files atomically; never remove the old model
        // before both downloads are verified. Identical pinned revisions only.
        for (_, name, _, _) in FILES {
            let source = fs::File::open(staging.path().join(name))?;
            let mut temp = tempfile::NamedTempFile::new_in(&dest)?;
            std::io::copy(&mut std::io::BufReader::new(source), &mut temp)?;
            temp.as_file().sync_all()?;
            temp.persist(dest.join(name))
                .context("Не удалось установить модель")?;
        }
    } else {
        fs::rename(staging.path(), &dest)?;
    }
    for (_, name, _, _) in FILES {
        fs::remove_file(root.join(format!(".e5-{name}.part")))?;
    }
    eprintln!("Поиск по смыслу: модель установлена");
    Ok(())
}
