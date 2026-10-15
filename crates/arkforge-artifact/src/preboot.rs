//! Closed DAYU200 acceptance initramfs derivative. The original gzip member is
//! retained byte-for-byte; a pinned compressed dependency archive makes its
//! toybox executable before system is mounted, then a newc overlay replaces
//! /init and adds the closed measurement script.
//! Linux accepts concatenated initramfs archives. No firmware code is patched.
use arkforge_core::digest::{Sha256Digest, sha256};

pub const ORIGINAL_SIZE: usize = 2_373_467;
pub const ORIGINAL_SHA256: &str =
    "af7d611f3a85dea26462f4cc58fec6a7c677e63245148f29ba8bbd22f705c236";
pub const SUPPORT_SIZE: usize = 1_534_719;
pub const SUPPORT_SHA256: &str = "e1d7bc7df36cf89e5c0c9640f1e502792c3ca39a14d67e48186028ad63b4fafe";
pub const SUPPORT_LIBRARY_SHA256: &str =
    "4b9875e386e3d82f737cb496ef85f4199ad39e9bc12942b4bad804e423776844";
pub const RESTORATION_SHA256: &str =
    "bdd1f4eacbe545f5a553b529980bc487723d37554148058dd551c241abf86de4";
pub const USERDATA_START: u64 = 10_217_324_544;
pub const USERDATA_BYTES: u64 = 1_468_006_400;
pub const MAX_IMAGE_BYTES: usize = 4 * 1024 * 1024;
const INIT: &str = include_str!("preboot-init.sh");
const HASH: &str = include_str!("preboot-hash.sh");

fn hex(s: &str, len: usize) -> bool {
    s.len() == len
        && s.bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
}

pub fn derive(original: &[u8], support: &[u8], nonce: &str) -> Result<Vec<u8>, &'static str> {
    derive_pinned(
        original,
        support,
        nonce,
        (ORIGINAL_SIZE, original_digest()),
        (SUPPORT_SIZE, support_digest()),
    )
}

fn derive_pinned(
    original: &[u8],
    support: &[u8],
    nonce: &str,
    base_pin: (usize, Sha256Digest),
    support_pin: (usize, Sha256Digest),
) -> Result<Vec<u8>, &'static str> {
    if original.len() != base_pin.0 || sha256(original) != base_pin.1 {
        return Err("preboot-original-ramdisk-not-pinned");
    }
    if support.len() != support_pin.0 || sha256(support) != support_pin.1 {
        return Err("preboot-support-archive-not-pinned");
    }
    if !hex(nonce, 32) {
        return Err("preboot-nonce-invalid");
    }
    let mut image = original.to_vec();
    while !image.len().is_multiple_of(4) {
        image.push(0);
    }
    image.extend_from_slice(support);
    while !image.len().is_multiple_of(4) {
        image.push(0);
    }
    image.extend(overlay(nonce));
    if image.len() > MAX_IMAGE_BYTES {
        return Err("preboot-image-too-large");
    }
    image.resize(MAX_IMAGE_BYTES, 0);
    Ok(image)
}

/// Clear the entire temporary overlay, including bytes beyond the original
/// gzip member. Merely rewriting the original file length leaves a live tail.
pub fn restoration(original: &[u8]) -> Result<Vec<u8>, &'static str> {
    if original.len() != ORIGINAL_SIZE || sha256(original) != original_digest() {
        return Err("preboot-original-ramdisk-not-pinned");
    }
    let mut image = original.to_vec();
    image.resize(MAX_IMAGE_BYTES, 0);
    if sha256(&image) != restoration_digest() {
        return Err("preboot-restoration-pin-invalid");
    }
    Ok(image)
}

fn overlay(nonce: &str) -> Vec<u8> {
    let mut out = Vec::new();
    cpio(&mut out, "init", 0o100755, INIT.as_bytes(), 1);
    cpio(
        &mut out,
        "arkforge-preboot",
        0o100700,
        HASH.replace("@NONCE@", nonce).as_bytes(),
        2,
    );
    cpio(
        &mut out,
        "arkforge-preboot-nonce",
        0o100400,
        nonce.as_bytes(),
        3,
    );
    cpio(&mut out, "TRAILER!!!", 0, b"", 4);
    out
}

fn cpio(out: &mut Vec<u8>, name: &str, mode: u32, data: &[u8], ino: u32) {
    out.extend_from_slice(b"070701");
    for value in [
        ino,
        mode,
        0,
        0,
        1,
        0,
        data.len() as u32,
        0,
        0,
        0,
        0,
        name.len() as u32 + 1,
        0,
    ] {
        out.extend_from_slice(format!("{value:08x}").as_bytes());
    }
    out.extend_from_slice(name.as_bytes());
    out.push(0);
    while !out.len().is_multiple_of(4) {
        out.push(0);
    }
    out.extend_from_slice(data);
    while !out.len().is_multiple_of(4) {
        out.push(0);
    }
}

/// Recognition is byte-exact, including the pinned base, entry order, file
/// modes, padding and complete scripts. Caller-provided scripts are refused.
pub fn inspect(image: &[u8]) -> Result<String, &'static str> {
    inspect_pinned(
        image,
        (ORIGINAL_SIZE, original_digest()),
        (SUPPORT_SIZE, support_digest()),
    )
}

fn inspect_pinned(
    image: &[u8],
    base_pin: (usize, Sha256Digest),
    support_pin: (usize, Sha256Digest),
) -> Result<String, &'static str> {
    let support_start = (base_pin.0 + 3) & !3;
    let support_end = support_start + support_pin.0;
    let script_start = (support_end + 3) & !3;
    if image.len() <= script_start || image.len() > MAX_IMAGE_BYTES {
        return Err("preboot-image-invalid");
    }
    let marker = b"nonce=";
    let tail = &image[script_start..];
    let at = tail
        .windows(marker.len())
        .position(|v| v == marker)
        .ok_or("preboot-nonce-missing")?
        + marker.len();
    let nonce = std::str::from_utf8(tail.get(at..at + 32).ok_or("preboot-nonce-missing")?)
        .map_err(|_| "preboot-nonce-invalid")?;
    if derive_pinned(
        &image[..base_pin.0],
        &image[support_start..support_end],
        nonce,
        base_pin,
        support_pin,
    )? != image
    {
        return Err("preboot-image-not-exact-derivative");
    }
    Ok(nonce.into())
}

pub fn original_digest() -> Sha256Digest {
    Sha256Digest::parse_hex(ORIGINAL_SHA256).expect("pinned SHA-256")
}

pub fn support_digest() -> Sha256Digest {
    Sha256Digest::parse_hex(SUPPORT_SHA256).expect("pinned support SHA-256")
}

pub fn restoration_digest() -> Sha256Digest {
    Sha256Digest::parse_hex(RESTORATION_SHA256).expect("pinned restoration SHA-256")
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn exact_overlay_round_trip_rejects_base_script_mode_padding_and_trailing_changes() {
        let base = b"synthetic compressed base";
        let digest = sha256(base);
        let support = b"synthetic pinned compressed dependency";
        let base_pin = (base.len(), digest);
        let support_pin = (support.len(), sha256(support));
        let nonce = "0123456789abcdef0123456789abcdef";
        let image = derive_pinned(base, support, nonce, base_pin, support_pin).unwrap();
        assert_eq!(
            inspect_pinned(&image, base_pin, support_pin).unwrap(),
            nonce
        );
        for index in [0, base.len(), base.len() + 14, image.len() - 1] {
            let mut changed = image.clone();
            changed[index] ^= 1;
            assert!(inspect_pinned(&changed, base_pin, support_pin).is_err());
        }
        let mut changed = image.clone();
        changed.push(0);
        assert!(inspect_pinned(&changed, base_pin, support_pin).is_err());
        let mut missing = image.clone();
        missing.drain(base.len()..base.len() + support.len());
        assert!(inspect_pinned(&missing, base_pin, support_pin).is_err());
        assert!(
            derive_pinned(base, b"different dependency", nonce, base_pin, support_pin).is_err()
        );
        let script_at = image.windows(10).position(|v| v == b"set -o pip").unwrap();
        let mut changed = image;
        changed[script_at] = b'#';
        assert!(inspect_pinned(&changed, base_pin, support_pin).is_err());
    }
    #[test]
    fn rejects_unpinned_bases_and_script_injection() {
        assert!(
            derive(
                &vec![0; ORIGINAL_SIZE],
                &[],
                "0123456789abcdef0123456789abcdef"
            )
            .is_err()
        );
        assert!(inspect(&vec![0; ORIGINAL_SIZE + 100]).is_err());
        assert!(!hex("0123456789abcdef0123456789abcde;id", 32));
    }
    #[test]
    fn overlay_has_regular_init_and_only_read_only_block_commands() {
        let bytes = overlay("0123456789abcdef0123456789abcdef");
        assert_eq!(&bytes[..6], b"070701");
        assert_eq!(&bytes[14..22], b"000081ed");
        assert!(INIT.contains("exec /bin/init_early"));
        assert!(!HASH.contains("of="));
        assert!(!HASH.contains(ORIGINAL_SHA256));
        assert!(HASH.contains("set -o pipefail"));
    }
}
