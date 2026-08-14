//! Deterministic fixture archives for contract tests.
//!
//! The pinned DAYU200 `images.tar.gz` is a ~700 MiB vendor blob that lives in
//! neither repository, so parity against it is structural: these fixtures carry
//! the *same member inventory, the same roles and the same partition table* as
//! ArkDeck's pinned decode, with small deterministic bodies. Byte-level parity
//! with the vendor archive is an evidence item, not something a fixture can
//! claim (architecture.md 24.1).
//!
//! Compression uses DEFLATE stored blocks. That keeps the fixture builder to a
//! framing writer instead of a second compressor implementation, and the result
//! is a conforming gzip stream that the system `gunzip` also accepts — which
//! the tests check.

use crate::inflate::Crc32;

const BLOCK: usize = 512;

/// The exact `CMDLINE` of the pinned DAYU200 archive, reconstructed from
/// ArkDeck `partition-mapping.json` (`arkdeck-dayu200-partition-mapping-1.0.0`,
/// archive `fc7637f3…`).
pub const PINNED_CMDLINE: &str = concat!(
    "CMDLINE:mtdparts=rk29xxnand:",
    "0x00002000@0x00002000(uboot),",
    "0x00002000@0x00004000(misc),",
    "0x00001000@0x00006000(bootctrl),",
    "0x00003000@0x00007000(resource),",
    "0x00030000@0x0000A000(boot_linux:bootable),",
    "0x00002000@0x0003A000(ramdisk),",
    "0x00400000@0x0003C000(system),",
    "0x00200000@0x0043C000(vendor),",
    "0x00019000@0x0063C000(sys-prod),",
    "0x00019000@0x00655000(chip-prod),",
    "0x00010000@0x0066E000(updater),",
    "0x00008000@0x0067E000(eng_system),",
    "0x00008000@0x00686000(eng_chipset),",
    "0x00020000@0x0069E000(chip_ckm),",
    "-@0x01308000(userdata:grow)"
);

/// The 17 members of the pinned archive, in the order ArkDeck's
/// `member-inventory.json` records them.
pub const PINNED_MEMBER_NAMES: [&str; 17] = [
    "boot_linux.img",
    "chip_ckm.img",
    "chip_prod.img",
    "config.cfg",
    "daily_build.log",
    "manifest_tag.xml",
    "MiniLoaderAll.bin",
    "parameter.txt",
    "ramdisk.img",
    "resource.img",
    "sys_prod.img",
    "system.img",
    "uboot.img",
    "updater_binary",
    "updater.img",
    "userdata.img",
    "vendor.img",
];

/// The build facts the fixture embeds in `system.img`, matching the values the
/// booted DAYU200 answered on 2026-08-04 (ArkDeck `RockchipFlashProfile`).
pub const FIXTURE_BUILD_VERSION: &str = "OpenHarmony-7.0.0.36";
pub const FIXTURE_PRODUCT_MODEL: &str = "ohos";

/// Builds ustar archives of regular files.
#[derive(Debug, Default)]
pub struct TarArchiveBuilder {
    bytes: Vec<u8>,
}

impl TarArchiveBuilder {
    pub fn new() -> Self {
        TarArchiveBuilder { bytes: Vec::new() }
    }

    pub fn add_file(mut self, path: &str, body: &[u8]) -> Self {
        let mut header = [0u8; BLOCK];
        let name = path.as_bytes();
        assert!(name.len() <= 100, "fixture paths stay inside the ustar name field");
        header[..name.len()].copy_from_slice(name);
        write_octal(&mut header[100..108], 0o644);
        write_octal(&mut header[108..116], 0);
        write_octal(&mut header[116..124], 0);
        write_octal(&mut header[124..136], body.len() as u64);
        write_octal(&mut header[136..148], 0);
        header[156] = b'0';
        header[257..263].copy_from_slice(b"ustar\0");
        header[263..265].copy_from_slice(b"00");
        for slot in header[148..156].iter_mut() {
            *slot = b' ';
        }
        let sum: u64 = header.iter().map(|byte| *byte as u64).sum();
        write_octal(&mut header[148..155], sum);
        header[155] = b' ';

        self.bytes.extend_from_slice(&header);
        self.bytes.extend_from_slice(body);
        let padding = (BLOCK - body.len() % BLOCK) % BLOCK;
        self.bytes.extend_from_slice(&vec![0u8; padding]);
        self
    }

    pub fn finish(self) -> Vec<u8> {
        let mut out = self.bytes;
        out.extend_from_slice(&[0u8; BLOCK * 2]);
        out
    }
}

fn write_octal(field: &mut [u8], value: u64) {
    let text = format!("{:0width$o}", value, width = field.len() - 1);
    field[..text.len()].copy_from_slice(text.as_bytes());
    field[field.len() - 1] = 0;
}

/// Wraps `data` in a gzip container using DEFLATE stored blocks.
pub fn gzip_stored(data: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(data.len() + data.len() / 65535 * 5 + 32);
    // Header: magic, CM=deflate, no flags, no mtime, no extra flags, OS=unknown.
    out.extend_from_slice(&[0x1f, 0x8b, 0x08, 0x00, 0, 0, 0, 0, 0x00, 0xff]);

    if data.is_empty() {
        out.extend_from_slice(&[0x01, 0x00, 0x00, 0xff, 0xff]);
    } else {
        let mut offset = 0usize;
        while offset < data.len() {
            let chunk = (data.len() - offset).min(0xffff);
            let is_final = offset + chunk == data.len();
            out.push(if is_final { 0x01 } else { 0x00 });
            out.extend_from_slice(&(chunk as u16).to_le_bytes());
            out.extend_from_slice(&(!(chunk as u16)).to_le_bytes());
            out.extend_from_slice(&data[offset..offset + chunk]);
            offset += chunk;
        }
    }

    let mut crc = Crc32::new();
    crc.update(data);
    out.extend_from_slice(&crc.finalize().to_le_bytes());
    out.extend_from_slice(&((data.len() as u64 & 0xffff_ffff) as u32).to_le_bytes());
    out
}

/// A deterministic body for a fixture member.
///
/// Distinct per member so a manifest that mixed two members up would fail on
/// the hashes, not just on the names.
pub fn fixture_body(name: &str, length: usize) -> Vec<u8> {
    let seed = name
        .bytes()
        .fold(0x811c_9dc5u32, |acc, byte| (acc ^ byte as u32).wrapping_mul(16_777_619));
    let mut state = seed;
    (0..length)
        .map(|_| {
            state = state.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
            (state >> 24) as u8
        })
        .collect()
}

/// The pinned DAYU200 archive shape: 17 members, real partition table, build
/// facts embedded inside `system.img`.
pub fn dayu200_archive() -> Vec<u8> {
    dayu200_archive_with(|_, body| body)
}

/// Same, with a hook to perturb one member — for negative tests.
pub fn dayu200_archive_with(mut mutate: impl FnMut(&str, Vec<u8>) -> Vec<u8>) -> Vec<u8> {
    let mut builder = TarArchiveBuilder::new();
    for name in PINNED_MEMBER_NAMES {
        let body = match name {
            "parameter.txt" => format!(
                "FIRMWARE_VER:1.0.0\nMACHINE_MODEL:RK3568\nMACHINE_ID:007\n{PINNED_CMDLINE}\n"
            )
            .into_bytes(),
            "system.img" => {
                let mut body = fixture_body(name, 4096);
                let properties = format!(
                    "const.product.model={FIXTURE_PRODUCT_MODEL}\nconst.ohos.fullname={FIXTURE_BUILD_VERSION}\n"
                );
                body.extend_from_slice(properties.as_bytes());
                body.extend_from_slice(&fixture_body("system.img.tail", 4096));
                body
            }
            _ => fixture_body(name, 2048),
        };
        builder = builder.add_file(name, &mutate(name, body));
    }
    gzip_stored(&builder.finish())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::{Read, Write};
    use std::process::{Command, Stdio};

    #[test]
    fn the_stored_block_writer_produces_a_stream_system_gunzip_accepts() {
        let payload = fixture_body("cross-check", 200_000);
        let compressed = gzip_stored(&payload);
        let Ok(mut child) = Command::new("gunzip")
            .arg("-c")
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()
        else {
            return;
        };
        child
            .stdin
            .as_mut()
            .unwrap()
            .write_all(&compressed)
            .unwrap();
        let output = child.wait_with_output().unwrap();
        assert!(output.status.success(), "system gunzip rejected the fixture");
        assert_eq!(output.stdout, payload);
    }

    #[test]
    fn our_own_reader_round_trips_the_fixture() {
        let payload = fixture_body("round-trip", 150_000);
        let compressed = gzip_stored(&payload);
        let mut reader = crate::inflate::GzipReader::new(compressed.as_slice()).unwrap();
        let mut decoded = Vec::new();
        reader.read_to_end(&mut decoded).unwrap();
        assert_eq!(decoded, payload);
        assert!(reader.trailer_verified());
    }

    #[test]
    fn fixture_bodies_differ_between_members() {
        assert_ne!(fixture_body("system.img", 64), fixture_body("vendor.img", 64));
    }
}
