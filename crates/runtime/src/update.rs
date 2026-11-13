//! Release validation. Inno Setup owns file installation.
use anyhow::{Context, Result, ensure};
use serde::Deserialize;
use sha2::{Digest, Sha256};
use std::{fs::File, io::Read, path::Path};

pub const REPOSITORY: &str = "https://github.com/IvanKhramov/notetaker";

/// Hex Ed25519 public key whose owner signs the installer checksum of every release.
/// The checksum sits next to the installer, so alone it proves integrity, not origin.
/// `None` until the key exists: see docs/release-signing.md.
pub const SIGNING_KEY: Option<&str> = None;

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
    /// The macOS disk image; a release published before it was uploaded has none.
    pub fn dmg(&self) -> Result<&Asset> {
        let version = self
            .tag_name
            .strip_prefix('v')
            .context("Некорректный тег релиза")?;
        semver::Version::parse(version)?;
        let name = format!("notetaker-{version}-macos-arm64.dmg");
        self.assets
            .iter()
            .find(|a| a.name == name)
            .context("Нет образа для macOS")
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
    /// The detached signature of a checksum file from this release.
    pub fn signature(&self, checksum: &Asset) -> Result<&Asset> {
        let name = format!("{}.sig", checksum.name);
        let signature = self
            .assets
            .iter()
            .find(|a| a.name == name)
            .context("У релиза нет подписи установщика")?;
        ensure!(
            signature.browser_download_url
                == format!("{REPOSITORY}/releases/download/{}/{name}", self.tag_name),
            "Недопустимый URL файла релиза"
        );
        Ok(signature)
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

fn unhex(text: &str) -> Result<Vec<u8>> {
    let text = text.trim();
    ensure!(
        text.len().is_multiple_of(2) && text.bytes().all(|b| b.is_ascii_hexdigit()),
        "Некорректная подпись релиза"
    );
    Ok((0..text.len())
        .step_by(2)
        .map(|i| u8::from_str_radix(&text[i..i + 2], 16).expect("hex digits"))
        .collect())
}

/// Checks a hex Ed25519 signature of `message` against a hex public key.
pub fn verify_signature(key: &str, message: &[u8], signature: &str) -> Result<()> {
    ring::signature::UnparsedPublicKey::new(&ring::signature::ED25519, unhex(key)?)
        .verify(message, &unhex(signature)?)
        .map_err(|_| anyhow::anyhow!("Подпись релиза не совпадает"))
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
    fn macos_release_needs_its_disk_image() {
        let mut r = release("v0.8.0");
        assert!(r.dmg().is_err());
        r.assets.push(Asset {
            name: "notetaker-0.8.0-macos-arm64.dmg".into(),
            browser_download_url: String::new(),
        });
        assert!(r.dmg().is_ok());
    }
    #[test]
    fn signature_binds_the_checksum_to_the_key() {
        use ring::signature::KeyPair;
        let hex = |bytes: &[u8]| bytes.iter().map(|b| format!("{b:02x}")).collect::<String>();
        let random = ring::rand::SystemRandom::new();
        let pkcs8 = ring::signature::Ed25519KeyPair::generate_pkcs8(&random).unwrap();
        let pair = ring::signature::Ed25519KeyPair::from_pkcs8(pkcs8.as_ref()).unwrap();
        let key = hex(pair.public_key().as_ref());
        let message = b"ba78  notetaker-v0.8.0-windows-x64-setup.exe\n";
        let signature = hex(pair.sign(message).as_ref());
        assert!(verify_signature(&key, message, &format!("{signature}\n")).is_ok());
        assert!(verify_signature(&key, b"other", &signature).is_err());
        assert!(verify_signature(&key, message, "zz").is_err());
        assert!(verify_signature(&key, message, "").is_err());

        let mut r = release("v0.8.0");
        let checksum = Asset {
            name: "notetaker-v0.8.0-windows-x64-setup.exe.sha256".into(),
            browser_download_url: String::new(),
        };
        assert!(r.signature(&checksum).is_err());
        r.assets.push(Asset {
            name: format!("{}.sig", checksum.name),
            browser_download_url: format!(
                "{REPOSITORY}/releases/download/v0.8.0/{}.sig",
                checksum.name
            ),
        });
        assert!(r.signature(&checksum).is_ok());
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
