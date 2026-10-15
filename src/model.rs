use crate::{config::Paths, extract::content_hash};
use anyhow::{Context, Result, bail};
use std::{
    fs::{self, OpenOptions},
    io::{Read, Write},
    os::unix::fs::OpenOptionsExt,
    path::Path,
    time::Duration,
};

pub const REVISION: &str = "bfcd298762cc34d0357ece5ebdd31791a3a374d8";
pub const PIPELINE: &str = "embeddinggemma2-bfcd298-q8-llama4d756bc-mean-v1-readers1";
pub struct ModelFile {
    pub name: &'static str,
    pub bytes: u64,
    pub sha256: &'static str,
}
pub const FILES: [ModelFile; 2] = [
    ModelFile {
        name: "embeddinggemma-2-Q8_0.gguf",
        bytes: 309855456,
        sha256: "2188ac1deca4b77dffefd603c2776a9d76d9d74ec01841392982ebb840b09135",
    },
    ModelFile {
        name: "mmproj-embeddinggemma-2-Q8_0.gguf",
        bytes: 554821024,
        sha256: "c4a8a52691ecef40618438928bdf9e68379b854e24166f292592353db0aab64f",
    },
];

pub fn agent(timeout: Duration, redirects: u32, direct: bool) -> ureq::Agent {
    let config = ureq::Agent::config_builder()
        .tls_config(
            ureq::tls::TlsConfig::builder()
                .root_certs(ureq::tls::RootCerts::PlatformVerifier)
                .build(),
        )
        .timeout_global(Some(timeout))
        .max_redirects(redirects);
    // A local inference endpoint must remain local even when the desktop has
    // an HTTP proxy configured. Downloads and explicitly remote endpoints use it.
    let config = if direct { config.proxy(None) } else { config };
    config.build().into()
}

pub fn verify(path: &Path, file: &ModelFile) -> Result<bool> {
    verify_download(path, file.bytes, file.sha256)
}

pub fn verify_download(path: &Path, bytes: u64, sha256: &str) -> Result<bool> {
    if !path.is_file() {
        return Ok(false);
    }
    if fs::metadata(path)?.len() != bytes {
        return Ok(false);
    }
    Ok(content_hash(path)? == sha256)
}

pub fn ensure(paths: &Paths, mut progress: impl FnMut(&str, u64, u64) -> Result<()>) -> Result<()> {
    paths.init()?;
    let models = paths.data.join("models");
    for file in &FILES {
        let dest = models.join(file.name);
        progress("Verifying EmbeddingGemma 2", 0, 0)?;
        if verify(&dest, file)? {
            continue;
        }
        if dest.exists() {
            fs::remove_file(&dest)?;
        }
        let url = format!(
            "https://huggingface.co/ggml-org/embeddinggemma-2-GGUF/resolve/{REVISION}/{}",
            file.name
        );
        download_verified(&url, &dest, file.bytes, file.sha256, |done, total| {
            progress(file.name, done, total)
        })?;
    }
    Ok(())
}

pub fn download_verified(
    url: &str,
    dest: &Path,
    bytes: u64,
    sha256: &str,
    mut progress: impl FnMut(u64, u64) -> Result<()>,
) -> Result<()> {
    if bytes == 0
        || bytes > 2 * 1024 * 1024 * 1024
        || sha256.len() != 64
        || !sha256.bytes().all(|b| b.is_ascii_hexdigit())
    {
        bail!("Invalid download metadata");
    }
    let lock_path = dest.with_file_name(format!(
        "{}.lock",
        dest.file_name()
            .context("Download destination needs a filename")?
            .to_string_lossy()
    ));
    let download_lock = OpenOptions::new()
        .create(true)
        .truncate(false)
        .read(true)
        .write(true)
        .mode(0o600)
        .open(lock_path)?;
    download_lock.lock()?;
    if verify_download(dest, bytes, sha256)? {
        return Ok(());
    }
    let partial = dest.with_file_name(format!(
        "{}.part",
        dest.file_name()
            .context("Download destination needs a filename")?
            .to_string_lossy()
    ));
    if verify_download(&partial, bytes, sha256)? {
        fs::rename(partial, dest)?;
        return Ok(());
    }
    let mut offset = fs::metadata(&partial).map(|m| m.len()).unwrap_or(0);
    if offset >= bytes {
        fs::remove_file(&partial)?;
        offset = 0;
    }
    let client = agent(Duration::from_secs(3600), 8, false);
    let request = client.get(url).header("User-Agent", "GSeek/0.1");
    let request = if offset > 0 {
        request.header("Range", format!("bytes={offset}-"))
    } else {
        request
    };
    let mut response = request.call().context(
        "Cannot download EmbeddingGemma 2. Check your connection and choose Retry Setup.",
    )?;
    if response.status() == 200 {
        offset = 0;
    } else if response.status() == 206 {
        let expected = format!("bytes {offset}-");
        if !response
            .headers()
            .get("Content-Range")
            .and_then(|h| h.to_str().ok())
            .is_some_and(|s| s.starts_with(&expected))
        {
            bail!("Model server returned an invalid download range");
        }
    } else {
        bail!("Model server returned {}", response.status());
    }
    let mut output = OpenOptions::new()
        .create(true)
        .write(true)
        .truncate(offset == 0)
        .append(offset > 0)
        .mode(0o600)
        .open(&partial)?;
    let mut reader = response.body_mut().as_reader();
    let mut buffer = [0; 65536];
    loop {
        progress(offset, bytes)?;
        let n = reader.read(&mut buffer)?;
        if n == 0 {
            break;
        }
        offset += n as u64;
        if offset > bytes {
            bail!("Model download exceeds its expected size");
        }
        output.write_all(&buffer[..n])?;
    }
    output.sync_all()?;
    drop(output);
    if !verify_download(&partial, bytes, sha256)? {
        fs::remove_file(partial)?;
        bail!("Model checksum did not match. Retry Setup to download a verified copy.");
    }
    fs::rename(partial, dest)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn checksum_rejects_same_size_corruption() -> Result<()> {
        let d = tempfile::tempdir()?;
        let path = d.path().join("model");
        fs::write(&path, b"abc")?;
        let file = ModelFile {
            name: "model",
            bytes: 3,
            sha256: "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad",
        };
        assert!(verify(&path, &file)?);
        fs::write(&path, b"xyz")?;
        assert!(!verify(&path, &file)?);
        Ok(())
    }
}
