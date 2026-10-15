use crate::{config::Settings, store::FileRecord};
use anyhow::{Context, Result, bail};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::{
    fs::{self, File},
    io::Read,
    path::{Path, PathBuf},
    process::{Command, Stdio},
    time::{Duration, Instant},
};

pub const READER_VERSION: &str = "readers-2";
pub const TEXT_CHARS: usize = 3200;
pub const MAX_TEXT_BYTES: u64 = 32 * 1024 * 1024;

fn media_command(name: &str) -> Command {
    let mut command = Command::new(tool(name));
    // Probing a local file can otherwise follow a playlist to a network URL.
    // Indexing media must never initiate its own network requests.
    command.args(["-protocol_whitelist", "file,pipe"]);
    command
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "lowercase")]
pub enum Input {
    Text { text: String },
    Image { path: PathBuf },
    Audio { path: PathBuf },
}

#[derive(Clone, Debug)]
pub struct Part {
    pub input: Input,
    pub text: String,
    pub page: Option<u32>,
    pub seconds: Option<f64>,
}

pub fn content_hash(path: &Path) -> Result<String> {
    let mut file = File::open(path)?;
    let mut hash = Sha256::new();
    let mut buffer = [0; 65536];
    loop {
        let read = file.read(&mut buffer)?;
        if read == 0 {
            break;
        }
        hash.update(&buffer[..read]);
    }
    Ok(hash.finalize().iter().map(|b| format!("{b:02x}")).collect())
}

pub fn text_parts(text: &str) -> Vec<String> {
    let chars: Vec<_> = text.chars().collect();
    let mut result = vec![];
    let mut start = 0;
    while start < chars.len() {
        let end = (start + TEXT_CHARS).min(chars.len());
        let part: String = chars[start..end].iter().collect();
        if !part.trim().is_empty() {
            result.push(part);
        }
        if end == chars.len() {
            break;
        }
        start = end - 200;
    }
    result
}

pub fn text_document(_title: &str, text: &str) -> Input {
    // Filenames rank separately. Keeping them out of the vector lets moves/renames reuse content.
    Input::Text {
        text: format!("title: none | text: {text}"),
    }
}

pub fn skip_reason(file: &FileRecord, settings: &Settings) -> Option<String> {
    if file.size > settings.max_file_mb * 1024 * 1024 {
        return Some(format!(
            "File exceeds the configured {} MB limit",
            settings.max_file_mb
        ));
    }
    if settings.external.is_some()
        && ["image/", "audio/", "video/"]
            .iter()
            .any(|prefix| file.mime.starts_with(prefix))
    {
        return Some(
            "External endpoints support text only; use managed inference for this file".into(),
        );
    }
    if file.mime.starts_with("image/") && !settings.images {
        return Some("Image indexing is disabled".into());
    }
    if (file.mime.starts_with("audio/") || file.mime.starts_with("video/")) && !settings.audio_video
    {
        return Some("Audio and video indexing is disabled".into());
    }
    let ext = file
        .path
        .extension()
        .and_then(|s| s.to_str())
        .unwrap_or("")
        .to_lowercase();
    let supported = file.mime.starts_with("text/")
        || file.mime.starts_with("image/")
        || file.mime.starts_with("audio/")
        || file.mime.starts_with("video/")
        || matches!(
            ext.as_str(),
            "pdf"
                | "docx"
                | "odt"
                | "txt"
                | "md"
                | "rst"
                | "csv"
                | "log"
                | "json"
                | "yaml"
                | "yml"
                | "toml"
                | "rs"
                | "py"
                | "c"
                | "h"
                | "cpp"
                | "js"
                | "ts"
                | "css"
                | "xml"
                | "sh"
        );
    if supported {
        None
    } else {
        Some(format!(
            "Content reader unavailable for {}; filename remains searchable",
            file.mime
        ))
    }
}

/// Readers materialize one page/frame/window at a time. Temporary media disappears as soon as
/// the file is done; no thumbnails or decoded recordings accumulate in the application data.
pub fn read(
    file: &FileRecord,
    settings: &Settings,
    cache: &Path,
    mut consume: impl FnMut(Part) -> Result<()>,
) -> Result<()> {
    if file.size > settings.max_file_mb * 1024 * 1024 {
        bail!(
            "File exceeds the configured {} MB limit",
            settings.max_file_mb
        );
    }
    let temp = tempfile::tempdir_in(cache)?;
    let p = &file.path;
    let ext = p
        .extension()
        .and_then(|e| e.to_str())
        .unwrap_or("")
        .to_lowercase();
    let mut text = |text: String, page: Option<u32>| -> Result<()> {
        for chunk in text_parts(&text) {
            consume(Part {
                input: text_document(&file.name, &chunk),
                text: chunk,
                page,
                seconds: None,
            })?;
        }
        Ok(())
    };
    if file.mime.starts_with("text/")
        || matches!(
            ext.as_str(),
            "txt"
                | "md"
                | "rst"
                | "csv"
                | "log"
                | "json"
                | "yaml"
                | "yml"
                | "toml"
                | "rs"
                | "py"
                | "c"
                | "h"
                | "cpp"
                | "js"
                | "ts"
                | "css"
                | "xml"
                | "sh"
        )
    {
        let mut bytes = vec![];
        File::open(p)?
            .take(MAX_TEXT_BYTES + 1)
            .read_to_end(&mut bytes)?;
        if bytes.len() as u64 > MAX_TEXT_BYTES {
            bail!("Text file exceeds the 32 MB reader limit");
        }
        if bytes.contains(&0) {
            bail!("Binary content cannot be read as a text document");
        }
        return text(
            String::from_utf8(bytes).context("Text file must use UTF-8")?,
            None,
        );
    }
    if matches!(ext.as_str(), "docx" | "odt") {
        // Libarchive's tar safely extracts a single member to stdout; no archive paths are written to disk.
        let member = if ext == "docx" {
            "word/document.xml"
        } else {
            "content.xml"
        };
        let xml = run_output(
            Command::new(tool("bsdtar"))
                .args(["-xOf"])
                .arg(p)
                .arg(member),
            temp.path(),
            MAX_TEXT_BYTES,
        )?;
        return text(strip_xml(&String::from_utf8_lossy(&xml)), None);
    }
    if ext == "pdf" || file.mime == "application/pdf" {
        let info = run_output(Command::new(tool("pdfinfo")).arg(p), temp.path(), 64 * 1024)?;
        let pages = String::from_utf8_lossy(&info)
            .lines()
            .find_map(|s| {
                s.strip_prefix("Pages:")
                    .and_then(|s| s.trim().parse::<u32>().ok())
            })
            .context("Cannot determine PDF page count")?;
        if pages > 5000 {
            bail!("PDF exceeds the 5,000 page limit");
        }
        // Managed inference embeds each visual page, including scans. External
        // text-only endpoints receive extractable text, never an assumed image API.
        let mut found_text = false;
        for page in 1..=pages {
            let index = page.to_string();
            let words = run_output(
                Command::new(tool("pdftotext"))
                    .args(["-f", &index, "-l", &index, "-enc", "UTF-8"])
                    .arg(p)
                    .arg("-"),
                temp.path(),
                MAX_TEXT_BYTES,
            )?;
            let words = String::from_utf8_lossy(&words).into_owned();
            for chunk in text_parts(&words) {
                found_text = true;
                consume(Part {
                    input: text_document(&file.name, &chunk),
                    text: chunk,
                    page: Some(page),
                    seconds: None,
                })?;
            }
            if settings.external.is_some() {
                continue;
            }
            let image = temp.path().join("page.png");
            run_output(
                Command::new(tool("pdftoppm"))
                    .args([
                        "-f",
                        &index,
                        "-l",
                        &index,
                        "-singlefile",
                        "-scale-to",
                        "672",
                        "-png",
                    ])
                    .arg(p)
                    .arg(temp.path().join("page")),
                temp.path(),
                64 * 1024,
            )?;
            consume(Part {
                input: Input::Image {
                    path: image.clone(),
                },
                text: words.chars().take(2000).collect(),
                page: Some(page),
                seconds: None,
            })?;
            fs::remove_file(image)?;
        }
        if settings.external.is_some() && !found_text {
            bail!("This PDF has no extractable text. Use managed inference for scanned pages.");
        }
        return Ok(());
    }
    if file.mime.starts_with("image/") {
        if !settings.images {
            bail!("Image indexing is disabled");
        }
        let image = temp.path().join("image.png");
        ffmpeg_image(p, &image, None, temp.path())?;
        return consume(Part {
            input: Input::Image { path: image },
            text: String::new(),
            page: None,
            seconds: None,
        });
    }
    if file.mime.starts_with("audio/") || file.mime.starts_with("video/") {
        if !settings.audio_video {
            bail!("Audio and video indexing is disabled");
        }
        let info = run_output(
            media_command("ffprobe")
                .args([
                    "-v",
                    "error",
                    "-show_entries",
                    "format=duration:stream=codec_type",
                    "-of",
                    "json",
                ])
                .arg(p),
            temp.path(),
            1024 * 1024,
        )?;
        let info: serde_json::Value = serde_json::from_slice(&info)?;
        let duration = info["format"]["duration"]
            .as_str()
            .and_then(|s| s.parse::<f64>().ok())
            .context("Cannot determine media duration")?;
        if !duration.is_finite() || !(0.0..=86400.0).contains(&duration) {
            bail!("Media must be shorter than 24 hours");
        }
        let streams = info["streams"].as_array().context("No media streams")?;
        let audio = streams.iter().any(|s| s["codec_type"] == "audio");
        let video = streams.iter().any(|s| s["codec_type"] == "video");
        if audio {
            for start in (0..duration.ceil() as u64).step_by(25) {
                let output = temp.path().join("window.wav");
                run_output(
                    media_command("ffmpeg")
                        .args([
                            "-nostdin",
                            "-v",
                            "error",
                            "-y",
                            "-ss",
                            &start.to_string(),
                            "-i",
                        ])
                        .arg(p)
                        .args([
                            "-t",
                            "30",
                            "-vn",
                            "-ac",
                            "1",
                            "-ar",
                            "16000",
                            "-c:a",
                            "pcm_s16le",
                        ])
                        .arg(&output),
                    temp.path(),
                    64 * 1024,
                )?;
                consume(Part {
                    input: Input::Audio {
                        path: output.clone(),
                    },
                    text: String::new(),
                    page: None,
                    seconds: Some(start as f64),
                })?;
                fs::remove_file(output)?;
            }
        }
        if video {
            for start in (0..duration.ceil() as u64).step_by(10) {
                let image = temp.path().join("frame.png");
                ffmpeg_image(p, &image, Some(start), temp.path())?;
                consume(Part {
                    input: Input::Image {
                        path: image.clone(),
                    },
                    text: String::new(),
                    page: None,
                    seconds: Some(start as f64),
                })?;
                fs::remove_file(image)?;
            }
        }
        return Ok(());
    }
    bail!("No content reader for {}", file.mime)
}

fn ffmpeg_image(input: &Path, output: &Path, seconds: Option<u64>, temp: &Path) -> Result<()> {
    let mut cmd = media_command("ffmpeg");
    cmd.args(["-nostdin", "-v", "error", "-y"]);
    if let Some(s) = seconds {
        cmd.args(["-ss", &s.to_string()]);
    }
    cmd.arg("-i")
        .arg(input)
        .args([
            "-frames:v",
            "1",
            "-vf",
            "scale='min(672,iw)':'min(672,ih)':force_original_aspect_ratio=decrease",
            "-threads",
            "1",
        ])
        .arg(output);
    run_output(&mut cmd, temp, 64 * 1024)?;
    Ok(())
}

pub fn tool(name: &str) -> PathBuf {
    if let Some(dir) = std::env::var_os("GSEEK_TOOLS") {
        return PathBuf::from(dir).join(name);
    }
    PathBuf::from(name)
}

pub fn run_output(command: &mut Command, temp: &Path, limit: u64) -> Result<Vec<u8>> {
    use std::os::unix::process::CommandExt;
    let output = tempfile::tempfile_in(temp)?;
    let error = tempfile::tempfile_in(temp)?;
    command
        .stdin(Stdio::null())
        .stdout(output.try_clone()?)
        .stderr(error.try_clone()?);
    // SAFETY: only async-signal-safe libc calls execute between fork and exec.
    unsafe {
        command.pre_exec(move || {
            libc::prctl(libc::PR_SET_PDEATHSIG, libc::SIGKILL);
            libc::setpgid(0, 0);
            let cap = libc::rlimit {
                rlim_cur: 64 * 1024 * 1024,
                rlim_max: 64 * 1024 * 1024,
            };
            libc::setrlimit(libc::RLIMIT_FSIZE, &cap);
            Ok(())
        });
    }
    let mut child = command.spawn().with_context(|| {
        format!(
            "Cannot start {:?}; reinstall GSeek to restore its readers",
            command.get_program()
        )
    })?;
    let deadline = Instant::now() + Duration::from_secs(60);
    let status = loop {
        if let Some(status) = child.try_wait()? {
            break status;
        }
        if Instant::now() >= deadline {
            unsafe {
                libc::kill(-(child.id() as i32), libc::SIGKILL);
            }
            let _ = child.wait();
            bail!("Content reader timed out after 60 seconds");
        }
        std::thread::sleep(Duration::from_millis(20));
    };
    use std::io::{Seek, SeekFrom};
    let mut output = output;
    output.seek(SeekFrom::Start(0))?;
    if !status.success() {
        let mut error = error;
        error.seek(SeekFrom::Start(0))?;
        let mut text = String::new();
        error.take(4096).read_to_string(&mut text)?;
        bail!("Content reader failed: {}", text.trim());
    }
    let mut bytes = vec![];
    output.take(limit + 1).read_to_end(&mut bytes)?;
    if bytes.len() as u64 > limit {
        bail!("Content reader exceeded its output limit");
    }
    Ok(bytes)
}

fn strip_xml(xml: &str) -> String {
    let mut text = String::new();
    let mut tag = false;
    for ch in xml.chars() {
        match ch {
            '<' => {
                tag = true;
                text.push(' ');
            }
            '>' => tag = false,
            _ if !tag => text.push(ch),
            _ => (),
        }
    }
    text.replace("&amp;", "&")
        .replace("&lt;", "<")
        .replace("&gt;", ">")
        .replace("&quot;", "\"")
        .replace("&apos;", "'")
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn multilingual_chunking_is_utf8_safe_and_overlaps() {
        let text = "مرحبا Welt 🌍 ".repeat(800);
        let parts = text_parts(&text);
        assert!(parts.len() > 1);
        assert_eq!(parts[0].chars().count(), TEXT_CHARS);
        assert_eq!(
            parts[0].chars().skip(TEXT_CHARS - 200).collect::<String>(),
            parts[1].chars().take(200).collect::<String>()
        );
    }
    #[test]
    fn hashes_are_content_addressed() -> Result<()> {
        let d = tempfile::tempdir()?;
        let a = d.path().join("a");
        let b = d.path().join("b");
        fs::write(&a, b"content")?;
        fs::copy(&a, &b)?;
        assert_eq!(content_hash(&a)?, content_hash(&b)?);
        fs::write(&b, b"changed")?;
        assert_ne!(content_hash(&a)?, content_hash(&b)?);
        Ok(())
    }
    #[test]
    fn xml_documents_preserve_words_and_entities() {
        assert_eq!(
            strip_xml("<p>Deutsch &amp; العربية</p>").trim(),
            "Deutsch & العربية"
        );
    }
}
