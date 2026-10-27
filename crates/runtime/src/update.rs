//! Release validation. Inno Setup owns file installation.
use anyhow::{Context, Result, ensure};
use serde::Deserialize;
use sha2::{Digest, Sha256};
use std::{fs::File, io::Read, path::Path};

pub const REPOSITORY: &str = "https://github.com/IvanKhramov/notetaker";

#[derive(Clone, Debug, Deserialize)]
pub struct Asset {
    pub name: String,
    pub browser_download_url: String,
}
#[derive(Clone, Debug, Deserialize)]
pub struct Release {
    pub tag_name: String,
    pub html_url: String,
    pub assets: Vec<Asset>,
    pub draft: bool,
    pub prerelease: bool,
}
impl Release {
    pub fn newer_than(&self, current: &str) -> Result<bool> {
        let version = semver::Version::parse(self.tag_name.trim_start_matches('v'))?;
        Ok(!self.draft
            && !self.prerelease
            && version.pre.is_empty()
            && version
                .cmp_precedence(&semver::Version::parse(current)?)
                .is_gt())
    }
    pub fn assets(&self) -> Result<(&Asset, &Asset)> {
        let version = self
            .tag_name
            .strip_prefix('v')
            .context("Некорректный тег релиза")?;
        semver::Version::parse(version)?;
        let name = format!("notetaker-v{version}-windows-x64-setup.exe");
        let archive = self
            .assets
            .iter()
            .find(|a| a.name == name)
            .context("Нет Windows x64 установщика")?;
        let checksum = self
            .assets
            .iter()
            .find(|a| a.name == format!("{name}.sha256"))
            .context("Нет SHA256 установщика")?;
        for asset in [archive, checksum] {
            ensure!(
                asset.browser_download_url
                    == format!(
                        "{REPOSITORY}/releases/download/{}/{}",
                        self.tag_name, asset.name
                    ),
                "Недопустимый URL файла релиза"
            );
        }
        Ok((archive, checksum))
    }
    pub fn notes_url(&self) -> String {
        // Do not open arbitrary URLs supplied in a JSON response.
        let expected = format!("{REPOSITORY}/releases/tag/{}", self.tag_name);
        if self.html_url == expected {
            expected
        } else {
            format!("{REPOSITORY}/releases")
        }
    }
}

pub fn verify_sha256(path: &Path, checksum: &str, name: &str) -> Result<()> {
    let fields: Vec<_> = checksum.split_whitespace().collect();
    ensure!(
        fields.len() == 2 && fields[1].trim_start_matches('*') == name,
        "Некорректный файл SHA256"
    );
    ensure!(
        fields[0].len() == 64 && fields[0].bytes().all(|b| b.is_ascii_hexdigit()),
        "Некорректный SHA256"
    );
    let mut hash = Sha256::new();
    let mut file = File::open(path)?;
    let mut buffer = [0; 64 * 1024];
    loop {
        let n = file.read(&mut buffer)?;
        if n == 0 {
            break;
        }
        hash.update(&buffer[..n]);
    }
    ensure!(
        format!("{:x}", hash.finalize()).eq_ignore_ascii_case(fields[0]),
        "SHA256 установщика не совпадает"
    );
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    fn release(tag: &str) -> Release {
        Release {
            tag_name: tag.into(),
            html_url: format!("{REPOSITORY}/releases/tag/{tag}"),
            assets: Vec::new(),
            draft: false,
            prerelease: false,
        }
    }
    #[test]
    fn semver_order_is_numeric_and_excludes_prereleases() {
        assert!(release("v0.10.0").newer_than("0.9.9").unwrap());
        assert!(!release("v0.7.0").newer_than("0.7.0").unwrap());
        assert!(!release("v0.7.0+rebuild").newer_than("0.7.0").unwrap());
        assert!(!release("v0.8.0-rc.1").newer_than("0.7.0").unwrap());
        assert!(release("bad").newer_than("0.7.0").is_err());
        let mut draft = release("v1.0.0");
        draft.draft = true;
        assert!(!draft.newer_than("0.7.0").unwrap());
    }
    #[test]
    fn selects_exact_windows_assets_from_our_release() {
        let mut r = release("v0.8.0");
        for name in [
            "notetaker-v0.8.0-linux-x64.zip",
            "notetaker-v0.8.0-windows-x64-setup.exe",
            "notetaker-v0.8.0-windows-x64-setup.exe.sha256",
        ] {
            r.assets.push(Asset {
                name: name.into(),
                browser_download_url: format!("{REPOSITORY}/releases/download/v0.8.0/{name}"),
            });
        }
        assert_eq!(
            r.assets().unwrap().0.name,
            "notetaker-v0.8.0-windows-x64-setup.exe"
        );
        r.assets[1].browser_download_url = "https://example.com/evil".into();
        assert!(r.assets().is_err());
        r.assets.pop();
        assert!(r.assets().is_err());
    }
    #[test]
    fn verifies_hash_and_filename() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("test.zip");
        fs::write(&file, "abc").unwrap();
        let hash = "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad";
        assert!(verify_sha256(&file, &format!("{hash}  test.zip\n"), "test.zip").is_ok());
        fs::write(&file, "changed").unwrap();
        assert!(verify_sha256(&file, &format!("{hash} test.zip"), "test.zip").is_err());
        assert!(verify_sha256(&file, &format!("{hash} other.zip"), "test.zip").is_err());
        assert!(verify_sha256(&file, "invalid", "test.zip").is_err());
    }
}
