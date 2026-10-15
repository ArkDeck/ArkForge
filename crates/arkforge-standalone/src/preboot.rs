//! Host-only preparation; no transport, daemon, permit or device access.
use crate::StandaloneError;
use arkforge_artifact::preboot;
use arkforge_core::digest::sha256;
use arkforge_platform::{fill_random, protect_path, sync_directory};
use std::io::Write;
use std::path::Path;

pub struct PreparedPreboot {
    pub image: Vec<u8>,
    pub restoration: Vec<u8>,
    pub nonce: String,
    pub digest: String,
}

pub fn prepare(runtime: &Path, original: &[u8]) -> Result<PreparedPreboot, StandaloneError> {
    let fail = |e: &dyn std::fmt::Display| {
        StandaloneError::new("PREBOOT_ARTIFACT_FAILED", e.to_string(), 3, false)
    };
    let restoration = preboot::restoration(original).map_err(|e| fail(&e))?;
    let mut random = [0; 16];
    fill_random(&mut random).map_err(|e| fail(&e))?;
    let nonce = random
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect::<String>();
    let image = preboot::derive(original, &nonce).map_err(|e| fail(&e))?;
    let digest = sha256(&image).to_hex();
    let root = runtime.join("derived-artifacts").join("preboot");
    std::fs::create_dir_all(&root).map_err(|e| fail(&e))?;
    protect_path(&root, true).map_err(|e| fail(&e))?;
    let mut file = std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(root.join(format!("{digest}.img")))
        .map_err(|e| fail(&e))?;
    file.write_all(&image)
        .and_then(|_| file.sync_all())
        .map_err(|e| fail(&e))?;
    sync_directory(&root).map_err(|e| fail(&e))?;
    Ok(PreparedPreboot {
        image,
        restoration,
        nonce,
        digest,
    })
}
