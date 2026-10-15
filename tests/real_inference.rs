//! These tests intentionally load the real, checksum-verified model. CI runs them for CPU
//! and software Vulkan; release hardware jobs can run the identical contract for CUDA/ROCm.
use anyhow::Result;
use gseek::{
    config::{Paths, Settings},
    extract::{Input, Part},
    inference::Runtime,
    store::validate_vector,
};
use std::{fs, path::Path, process::Command};

fn checked(command: &mut Command) -> Result<()> {
    anyhow::ensure!(command.status()?.success(), "Fixture generator failed");
    Ok(())
}
fn cosine(a: &[f32], b: &[f32]) -> f32 {
    a.iter().zip(b).map(|(a, b)| a * b).sum()
}

#[test]
#[ignore = "Downloads/loads 865 MB of real model weights; run with --ignored"]
fn text_matches_pinned_reference_vector() -> Result<()> {
    let paths = Paths::from_env();
    paths.init()?;
    gseek::model::ensure(&paths, |_, _, _| Ok(()))?;
    let settings = paths.load()?;
    let mut runtime = Runtime::new(&paths, &settings, true);
    let vector = runtime.query("the clause about pets in the lease")?;
    validate_vector(&vector)?;
    let reference: Vec<f32> = serde_json::from_str(include_str!("reference-embedding.json"))?;
    let score = cosine(&vector, &reference);
    assert!(
        score > 0.999,
        "Embedding drift (cosine {score}). Keep the old runtime or explicitly version/rebuild the index."
    );
    Ok(())
}

#[test]
#[ignore = "Uses the real image/audio encoders and FFmpeg"]
fn multimodal_vectors_share_one_space() -> Result<()> {
    let paths = Paths::from_env();
    paths.init()?;
    gseek::model::ensure(&paths, |_, _, _| Ok(()))?;
    let settings = paths.load()?;
    let mut runtime = Runtime::new(&paths, &settings, false);
    let dir = tempfile::Builder::new().prefix("gseek-media-").tempdir()?;
    let image = dir.path().join("red-square.ppm");
    let mut bytes = b"P6\n128 128\n255\n".to_vec();
    for _ in 0..128 * 128 {
        bytes.extend_from_slice(&[240, 20, 20]);
    }
    fs::write(&image, bytes)?;
    let picture = runtime.embed(&Input::Image { path: image })?;
    validate_vector(&picture)?;
    let wav = dir.path().join("tone.wav");
    checked(
        Command::new("ffmpeg")
            .args([
                "-nostdin",
                "-v",
                "error",
                "-f",
                "lavfi",
                "-i",
                "sine=frequency=440:sample_rate=16000:duration=1",
                "-ac",
                "1",
            ])
            .arg(&wav),
    )?;
    let sound = runtime.embed(&Input::Audio { path: wav })?;
    validate_vector(&sound)?;
    assert!(
        cosine(&picture, &sound) < 0.99,
        "Different modalities must not produce a constant vector"
    );
    let text = runtime.query("a red square")?;
    validate_vector(&text)?;
    assert!(
        cosine(&text, &picture) > cosine(&text, &sound),
        "Visual retrieval must rank the matching image above an unrelated tone"
    );
    Ok(())
}

#[test]
#[ignore = "Uses installed native content readers"]
fn pdf_video_and_document_readers_keep_locations() -> Result<()> {
    let paths = Paths::from_env();
    paths.init()?;
    let dir = tempfile::Builder::new()
        .prefix("gseek-readers-")
        .tempdir()?;
    let mut settings = Settings {
        folders: vec![dir.path().to_owned()],
        ..Default::default()
    };
    settings.audio_video = true;
    let video = dir.path().join("clip.mp4");
    checked(
        Command::new("ffmpeg")
            .args([
                "-nostdin",
                "-v",
                "error",
                "-f",
                "lavfi",
                "-i",
                "color=red:size=128x128:duration=1",
                "-f",
                "lavfi",
                "-i",
                "sine=frequency=440:sample_rate=16000:duration=1",
                "-c:v",
                "mpeg4",
                "-c:a",
                "aac",
                "-shortest",
                "-threads",
                "1",
            ])
            .arg(&video),
    )?;
    let mut got = vec![];
    let record = gseek::discovery::record_from_uri(
        url::Url::from_file_path(&video).unwrap().as_str(),
        &settings,
    )?
    .unwrap();
    gseek::extract::read(&record, &settings, &paths.cache, |part: Part| {
        got.push((matches!(part.input, Input::Audio { .. }), part.seconds));
        Ok(())
    })?;
    assert!(got.contains(&(true, Some(0.0))));
    assert!(got.contains(&(false, Some(0.0))));
    // The PDF fixture is tiny, uncompressed, and reproducible; its visual reader must cover every page.
    let pdf = dir.path().join("lease.pdf");
    write_pdf(&pdf)?;
    let record = gseek::discovery::record_from_uri(
        url::Url::from_file_path(&pdf).unwrap().as_str(),
        &settings,
    )?
    .unwrap();
    let mut pages = vec![];
    gseek::extract::read(&record, &settings, &paths.cache, |part| {
        if matches!(part.input, Input::Image { .. }) {
            pages.push(part.page);
        }
        Ok(())
    })?;
    assert_eq!(pages, vec![Some(1), Some(2)]);
    Ok(())
}

fn write_pdf(path: &Path) -> Result<()> {
    let objects=["<< /Type /Catalog /Pages 2 0 R >>".to_string(),"<< /Type /Pages /Kids [3 0 R 5 0 R] /Count 2 >>".into(),
        "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 300 300] /Contents 4 0 R /Resources << /Font << /F1 7 0 R >> >> >>".into(),
        "<< /Length 56 >>\nstream\nBT /F1 16 Tf 20 200 Td (Pets are welcome.) Tj ET\nendstream".into(),
        "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 300 300] /Contents 6 0 R /Resources << /Font << /F1 7 0 R >> >> >>".into(),
        "<< /Length 57 >>\nstream\nBT /F1 16 Tf 20 200 Td (Payment is monthly.) Tj ET\nendstream".into(),
        "<< /Type /Font /Subtype /Type1 /BaseFont /Helvetica >>".into()];
    let mut bytes = b"%PDF-1.4\n".to_vec();
    let mut offsets = vec![0];
    for (i, obj) in objects.iter().enumerate() {
        offsets.push(bytes.len());
        bytes.extend_from_slice(format!("{} 0 obj\n{obj}\nendobj\n", i + 1).as_bytes());
    }
    let xref = bytes.len();
    bytes.extend_from_slice(
        format!("xref\n0 {}\n0000000000 65535 f \n", objects.len() + 1).as_bytes(),
    );
    for offset in offsets.into_iter().skip(1) {
        bytes.extend_from_slice(format!("{offset:010} 00000 n \n").as_bytes());
    }
    bytes.extend_from_slice(
        format!(
            "trailer\n<< /Size {} /Root 1 0 R >>\nstartxref\n{xref}\n%%EOF\n",
            objects.len() + 1
        )
        .as_bytes(),
    );
    fs::write(path, bytes)?;
    Ok(())
}
