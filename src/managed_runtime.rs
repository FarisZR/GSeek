//! Consume immutable upstream distributions. GSeek never builds or patches an engine.
use crate::{
    config::{Backend, Paths},
    model,
};
use anyhow::{Context, Result, bail};
use serde::Deserialize;
use std::{
    collections::BTreeMap,
    fs,
    os::unix::fs::OpenOptionsExt,
    path::PathBuf,
    process::{Command, Stdio},
};
#[derive(Deserialize)]
struct Manifest {
    version: u32,
    release: String,
    assets: BTreeMap<String, BTreeMap<String, Vec<Asset>>>,
}
#[derive(Deserialize)]
struct Asset {
    url: String,
    sha256: String,
    bytes: u64,
}

pub fn directory(paths: &Paths, backend: Backend) -> Result<PathBuf> {
    let name = format!("{backend:?}").to_lowercase();
    if let Some(dir) = std::env::var_os("GSEEK_RUNTIME_DIR") {
        let dir = PathBuf::from(dir).join(&name);
        if !dir.join("llama-server").is_file() {
            bail!("GSEEK_RUNTIME_DIR has no upstream {name}/llama-server");
        }
        return Ok(dir);
    }
    let manifest: Manifest = serde_json::from_str(include_str!("../data/runtimes.json"))?;
    if manifest.version != 1 {
        bail!("Unsupported runtime metadata");
    }
    let assets = manifest
        .assets
        .get(&name)
        .and_then(|a| a.get(std::env::consts::ARCH))
        .with_context(|| {
            format!("No upstream {name} runtime is available for this architecture")
        })?;
    let root = paths.data.join("runtimes");
    fs::create_dir_all(&root)?;
    let lock = fs::OpenOptions::new()
        .create(true)
        .truncate(false)
        .read(true)
        .write(true)
        .mode(0o600)
        .open(root.join(format!("{name}.lock")))?;
    lock.lock()?;
    let dest = root.join(format!(
        "{}-{name}-{}",
        manifest.release,
        std::env::consts::ARCH
    ));
    if dest.join("llama-server").is_file() {
        return Ok(dest);
    }
    let temp = tempfile::tempdir_in(&root)?;
    for asset in assets {
        let prefix = format!(
            "https://github.com/ggml-org/llama.cpp/releases/download/{}/",
            manifest.release
        );
        if !asset.url.starts_with(&prefix) {
            bail!("Invalid upstream runtime URL");
        }
        let archive = root.join(format!("{}.tar.gz", asset.sha256));
        model::download_verified(&asset.url, &archive, asset.bytes, &asset.sha256, |_, _| {
            Ok(())
        })
        .with_context(|| {
            format!("Cannot download the {name} runtime; check your connection and retry setup")
        })?;
        let status = Command::new(crate::extract::tool("bsdtar"))
            .arg("-xf")
            .arg(&archive)
            .arg("-C")
            .arg(temp.path())
            .args([
                "--strip-components",
                "1",
                "--no-same-owner",
                "--no-same-permissions",
            ])
            .stdin(Stdio::null())
            .status()?;
        if !status.success() {
            bail!("Cannot unpack the verified upstream {name} runtime");
        }
        fs::remove_file(archive)?;
    }
    if !temp.path().join("llama-server").is_file() {
        bail!("Upstream runtime is missing llama-server");
    }
    fs::rename(temp.path(), &dest)?;
    Ok(dest)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    #[ignore = "Downloads official runtime archives, including CUDA libraries; no GPU or SDK required"]
    fn official_runtime_distributions_install_and_link() -> Result<()> {
        let paths = Paths::from_env();
        paths.init()?;
        for backend in [Backend::Cpu, Backend::Vulkan, Backend::Cuda] {
            let dir = directory(&paths, backend)?;
            let version = Command::new(dir.join("llama-server"))
                .arg("--version")
                .env("LD_LIBRARY_PATH", &dir)
                .output()?;
            anyhow::ensure!(
                version.status.success(),
                "{backend:?}: {}",
                String::from_utf8_lossy(&version.stderr)
            );
            let plugin = match backend {
                Backend::Vulkan => Some("libggml-vulkan.so"),
                Backend::Cuda => Some("libggml-cuda.so"),
                _ => None,
            };
            if let Some(plugin) = plugin {
                let links = Command::new("ldd")
                    .arg(dir.join(plugin))
                    .env("LD_LIBRARY_PATH", &dir)
                    .output()?;
                anyhow::ensure!(
                    links.status.success(),
                    "Cannot inspect {backend:?} libraries"
                );
                for line in String::from_utf8_lossy(&links.stdout).lines() {
                    // libcuda belongs to the host NVIDIA driver. Everything
                    // else must resolve from upstream archives or the host ABI.
                    anyhow::ensure!(
                        !line.contains("not found")
                            || line.trim_start().starts_with("libcuda.so.1 "),
                        "Missing {backend:?} dependency: {line}"
                    );
                }
            }
            println!(
                "Verified upstream {backend:?} installation on {}",
                std::env::consts::ARCH
            );
        }
        Ok(())
    }
}
