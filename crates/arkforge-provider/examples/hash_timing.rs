//! Measures the exact pre-write image validation path used by RockUSB.

use arkforge_core::Sha256Digest;
use arkforge_provider::rockchip_execute::StagedImage;
use std::path::PathBuf;
use std::time::Instant;

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let mut arguments = std::env::args().skip(1);
    let path = PathBuf::from(
        arguments
            .next()
            .ok_or("usage: hash_timing <path> <sha256>")?,
    );
    let sha256 = Sha256Digest::parse_hex(
        &arguments
            .next()
            .ok_or("usage: hash_timing <path> <sha256>")?,
    )?;
    if arguments.next().is_some() {
        return Err("usage: hash_timing <path> <sha256>".into());
    }
    let size_bytes = std::fs::metadata(&path)?.len();
    let image = StagedImage {
        member: path
            .file_name()
            .and_then(|name| name.to_str())
            .unwrap_or("image")
            .to_string(),
        path,
        size_bytes,
        sha256,
    };
    let started = Instant::now();
    let validated = image.open_and_revalidate()?;
    println!(
        "bytes={} duration_ms={} wall_ms={} backend={}",
        size_bytes,
        validated.validation_duration_ms(),
        started.elapsed().as_millis(),
        validated.validation_backend()
    );
    Ok(())
}
