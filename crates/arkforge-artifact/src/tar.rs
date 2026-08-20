//! Hardened streaming tar reader.
//!
//! architecture.md 10.4: reject absolute paths, `..`, symlinks, hardlinks and
//! device nodes; hash every member; never write to the filesystem while
//! parsing. The parser never extracts — it only observes — so a malicious
//! archive has no path to a host file at all.
//!
//! Rejection codes mirror the ArkDeck archive contract (`ARC001`..`ARC009`)
//! so migration parity can be compared code-for-code rather than prose-for-prose.

use arkforge_core::digest::{Sha256, Sha256Digest};
use core::fmt;
use std::collections::BTreeSet;
use std::io::{self, Read};

const BLOCK: usize = 512;
/// A member path longer than this is refused rather than allocated.
const MAX_PATH_LEN: usize = 1024;
/// Bound on a pax extended header, which is attacker-controlled length data.
const MAX_PAX_BYTES: usize = 1 << 20;

/// Why an archive was rejected.
///
/// The `code()` values are the ArkDeck contract codes; the payload carries the
/// specifics a receipt needs.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ArchiveError {
    /// ARC001: the archive is not the one the plan is bound to.
    IdentityMismatch {
        expected: String,
        observed: String,
    },
    /// ARC002: framing is not a valid archive.
    ArchiveInvalid(String),
    /// ARC003: a member path is absolute.
    PathAbsolute(String),
    /// ARC004: a member path escapes the archive root.
    PathTraversal(String),
    /// ARC005: a member path is otherwise unusable (backslash, NUL, control,
    /// non-UTF-8, over-long).
    PathInvalid(String),
    /// ARC006: two members share a path.
    PathDuplicate(String),
    /// ARC007: symlink or hardlink member.
    LinkUnsupported(String),
    /// ARC008: member type outside the supported set.
    MemberTypeUnsupported {
        path: String,
        type_flag: char,
    },
    /// ARC009: the declared member size does not match the bytes present.
    MemberSizeMismatch {
        path: String,
        declared: u64,
        observed: u64,
    },
    Io(String),
}

impl ArchiveError {
    pub fn code(&self) -> &'static str {
        match self {
            ArchiveError::IdentityMismatch { .. } => "ARC001_IDENTITY_MISMATCH",
            ArchiveError::ArchiveInvalid(_) => "ARC002_ARCHIVE_INVALID",
            ArchiveError::PathAbsolute(_) => "ARC003_PATH_ABSOLUTE",
            ArchiveError::PathTraversal(_) => "ARC004_PATH_TRAVERSAL",
            ArchiveError::PathInvalid(_) => "ARC005_PATH_INVALID",
            ArchiveError::PathDuplicate(_) => "ARC006_PATH_DUPLICATE",
            ArchiveError::LinkUnsupported(_) => "ARC007_LINK_UNSUPPORTED",
            ArchiveError::MemberTypeUnsupported { .. } => "ARC008_MEMBER_TYPE_UNSUPPORTED",
            ArchiveError::MemberSizeMismatch { .. } => "ARC009_MEMBER_SIZE_MISMATCH",
            ArchiveError::Io(_) => "ARC002_ARCHIVE_INVALID",
        }
    }
}

impl fmt::Display for ArchiveError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "[{}] ", self.code())?;
        match self {
            ArchiveError::IdentityMismatch { expected, observed } => write!(
                f,
                "archive identity mismatch: expected {expected}, observed {observed}"
            ),
            ArchiveError::ArchiveInvalid(detail) => write!(f, "invalid archive: {detail}"),
            ArchiveError::PathAbsolute(path) => write!(f, "absolute member path {path:?}"),
            ArchiveError::PathTraversal(path) => write!(f, "member path escapes root: {path:?}"),
            ArchiveError::PathInvalid(path) => write!(f, "unusable member path {path:?}"),
            ArchiveError::PathDuplicate(path) => write!(f, "duplicate member path {path:?}"),
            ArchiveError::LinkUnsupported(path) => {
                write!(f, "link member {path:?} is not supported")
            }
            ArchiveError::MemberTypeUnsupported { path, type_flag } => {
                write!(f, "member {path:?} has unsupported type flag {type_flag:?}")
            }
            ArchiveError::MemberSizeMismatch {
                path,
                declared,
                observed,
            } => write!(
                f,
                "member {path:?} declares {declared} bytes but carries {observed}"
            ),
            ArchiveError::Io(detail) => write!(f, "archive read failed: {detail}"),
        }
    }
}

impl std::error::Error for ArchiveError {}

impl From<io::Error> for ArchiveError {
    fn from(error: io::Error) -> Self {
        ArchiveError::Io(error.to_string())
    }
}

/// A member header, after validation.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TarMemberHeader {
    pub path: String,
    pub size: u64,
    pub mode: u32,
}

/// A fully observed member: its identity plus the hash of its bytes.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TarMemberObservation {
    pub path: String,
    pub size: u64,
    pub sha256: Sha256Digest,
}

/// Streaming tar reader over any byte source.
#[derive(Debug)]
pub struct TarReader<R: Read> {
    source: R,
    /// Bytes of the current member not yet consumed, including block padding.
    pending_body: u64,
    pending_padding: usize,
    seen_paths: BTreeSet<String>,
    finished: bool,
    /// Path override from a pax `path=` record or a GNU `L` header.
    next_path_override: Option<String>,
    next_size_override: Option<u64>,
}

impl<R: Read> TarReader<R> {
    pub fn new(source: R) -> Self {
        TarReader {
            source,
            pending_body: 0,
            pending_padding: 0,
            seen_paths: BTreeSet::new(),
            finished: false,
            next_path_override: None,
            next_size_override: None,
        }
    }

    /// Advances to the next regular-file member.
    ///
    /// Metadata members (pax, GNU long name) are consumed transparently.
    /// Anything else — link, directory, device, fifo — is a rejection, not a
    /// skip: an archive that carries one is not the shape a firmware bundle
    /// takes, and quietly ignoring it would let content hide from the manifest.
    pub fn next_member(&mut self) -> Result<Option<TarMemberHeader>, ArchiveError> {
        if self.finished {
            return Ok(None);
        }
        self.skip_pending()?;

        loop {
            let mut header = [0u8; BLOCK];
            match self.read_block(&mut header)? {
                BlockRead::Eof => {
                    return Err(ArchiveError::ArchiveInvalid(
                        "archive ends without the two-block end marker".into(),
                    ));
                }
                BlockRead::Short(count) => {
                    return Err(ArchiveError::ArchiveInvalid(format!(
                        "archive ends with a partial {count}-byte block"
                    )));
                }
                BlockRead::Full => {}
            }

            if header.iter().all(|byte| *byte == 0) {
                // First zero block: the second must follow, and nothing after.
                let mut second = [0u8; BLOCK];
                match self.read_block(&mut second)? {
                    BlockRead::Full if second.iter().all(|byte| *byte == 0) => {}
                    _ => {
                        return Err(ArchiveError::ArchiveInvalid(
                            "end marker is not two zero blocks".into(),
                        ));
                    }
                }
                self.check_no_trailing_data()?;
                self.finished = true;
                return Ok(None);
            }

            verify_checksum(&header)?;

            let type_flag = header[156] as char;
            let declared_size = parse_octal(&header[124..136], "size")?;

            match type_flag {
                'x' | 'X' => {
                    let records = self.read_body_to_vec(declared_size)?;
                    self.apply_pax(&records)?;
                    continue;
                }
                'g' => {
                    // Global metadata applies to the whole archive and carries
                    // no member content; nothing here needs it.
                    self.read_body_to_vec(declared_size)?;
                    continue;
                }
                'L' => {
                    let raw = self.read_body_to_vec(declared_size)?;
                    let name = String::from_utf8(strip_nul(&raw).to_vec()).map_err(|_| {
                        ArchiveError::PathInvalid("<non-utf8 gnu long name>".into())
                    })?;
                    self.next_path_override = Some(name);
                    continue;
                }
                'K' => {
                    self.read_body_to_vec(declared_size)?;
                    continue;
                }
                _ => {}
            }

            let raw_path = match self.next_path_override.take() {
                Some(path) => path,
                None => header_path(&header)?,
            };
            let size = self.next_size_override.take().unwrap_or(declared_size);
            let path = validate_path(&raw_path)?;

            match type_flag {
                '0' | '\0' | '7' => {}
                '1' | '2' => return Err(ArchiveError::LinkUnsupported(path)),
                other => {
                    return Err(ArchiveError::MemberTypeUnsupported {
                        path,
                        type_flag: other,
                    });
                }
            }

            if !self.seen_paths.insert(path.clone()) {
                return Err(ArchiveError::PathDuplicate(path));
            }

            let mode = parse_octal(&header[100..108], "mode")? as u32;
            self.pending_body = size;
            self.pending_padding = padding_for(size);
            return Ok(Some(TarMemberHeader { path, size, mode }));
        }
    }

    /// Streams the current member's body through `sink`, returning its digest.
    ///
    /// The body is never buffered whole: a 2 GiB member hashes in 64 KiB
    /// chunks (architecture.md 10.4).
    pub fn read_member_body(
        &mut self,
        header: &TarMemberHeader,
        mut sink: impl FnMut(&[u8]),
    ) -> Result<TarMemberObservation, ArchiveError> {
        let mut hasher = Sha256::new();
        let mut remaining = self.pending_body;
        let mut observed = 0u64;
        let mut chunk = vec![0u8; 64 * 1024];
        while remaining > 0 {
            let want = chunk.len().min(remaining as usize);
            let count = self.source.read(&mut chunk[..want])?;
            if count == 0 {
                return Err(ArchiveError::MemberSizeMismatch {
                    path: header.path.clone(),
                    declared: header.size,
                    observed,
                });
            }
            hasher.update(&chunk[..count]);
            sink(&chunk[..count]);
            observed += count as u64;
            remaining -= count as u64;
        }
        self.pending_body = 0;
        self.consume_padding()?;
        Ok(TarMemberObservation {
            path: header.path.clone(),
            size: header.size,
            sha256: hasher.finalize(),
        })
    }

    fn skip_pending(&mut self) -> Result<(), ArchiveError> {
        if self.pending_body > 0 {
            let mut remaining = self.pending_body;
            let mut chunk = vec![0u8; 64 * 1024];
            while remaining > 0 {
                let want = chunk.len().min(remaining as usize);
                let count = self.source.read(&mut chunk[..want])?;
                if count == 0 {
                    return Err(ArchiveError::ArchiveInvalid(
                        "archive ends inside a member body".into(),
                    ));
                }
                remaining -= count as u64;
            }
            self.pending_body = 0;
        }
        self.consume_padding()
    }

    fn consume_padding(&mut self) -> Result<(), ArchiveError> {
        if self.pending_padding == 0 {
            return Ok(());
        }
        let mut padding = vec![0u8; self.pending_padding];
        let mut filled = 0usize;
        while filled < padding.len() {
            let count = self.source.read(&mut padding[filled..])?;
            if count == 0 {
                return Err(ArchiveError::ArchiveInvalid(
                    "archive ends inside member block padding".into(),
                ));
            }
            filled += count;
        }
        self.pending_padding = 0;
        Ok(())
    }

    fn read_body_to_vec(&mut self, size: u64) -> Result<Vec<u8>, ArchiveError> {
        if size > MAX_PAX_BYTES as u64 {
            return Err(ArchiveError::ArchiveInvalid(format!(
                "metadata member of {size} bytes exceeds the {MAX_PAX_BYTES}-byte bound"
            )));
        }
        let mut body = vec![0u8; size as usize];
        let mut filled = 0usize;
        while filled < body.len() {
            let count = self.source.read(&mut body[filled..])?;
            if count == 0 {
                return Err(ArchiveError::ArchiveInvalid(
                    "archive ends inside a metadata member".into(),
                ));
            }
            filled += count;
        }
        self.pending_padding = padding_for(size);
        self.consume_padding()?;
        Ok(body)
    }

    fn apply_pax(&mut self, records: &[u8]) -> Result<(), ArchiveError> {
        // "%d %s=%s\n" records (POSIX.1-2001). Only path and size change what
        // this parser reports; other keywords are timestamps and ownership,
        // which a firmware manifest does not consume.
        let mut cursor = 0usize;
        while cursor < records.len() {
            let space = records[cursor..]
                .iter()
                .position(|byte| *byte == b' ')
                .ok_or_else(|| {
                    ArchiveError::ArchiveInvalid("pax record has no length delimiter".into())
                })?;
            let length: usize = std::str::from_utf8(&records[cursor..cursor + space])
                .ok()
                .and_then(|text| text.parse().ok())
                .ok_or_else(|| {
                    ArchiveError::ArchiveInvalid("pax record length is not a number".into())
                })?;
            if length < space + 2 || cursor + length > records.len() {
                return Err(ArchiveError::ArchiveInvalid(
                    "pax record length is out of range".into(),
                ));
            }
            let body = &records[cursor + space + 1..cursor + length - 1];
            if let Some(equals) = body.iter().position(|byte| *byte == b'=') {
                let key = &body[..equals];
                let value = &body[equals + 1..];
                match key {
                    b"path" => {
                        let text = std::str::from_utf8(value)
                            .map_err(|_| ArchiveError::PathInvalid("<non-utf8 pax path>".into()))?;
                        self.next_path_override = Some(text.to_string());
                    }
                    b"size" => {
                        let text = std::str::from_utf8(value).ok().and_then(|t| t.parse().ok());
                        self.next_size_override = Some(text.ok_or_else(|| {
                            ArchiveError::ArchiveInvalid("pax size is not a number".into())
                        })?);
                    }
                    b"linkpath" => {
                        return Err(ArchiveError::LinkUnsupported(
                            String::from_utf8_lossy(value).into_owned(),
                        ));
                    }
                    _ => {}
                }
            }
            cursor += length;
        }
        Ok(())
    }

    fn read_block(&mut self, out: &mut [u8; BLOCK]) -> Result<BlockRead, ArchiveError> {
        let mut filled = 0usize;
        while filled < BLOCK {
            let count = self.source.read(&mut out[filled..])?;
            if count == 0 {
                return Ok(if filled == 0 {
                    BlockRead::Eof
                } else {
                    BlockRead::Short(filled)
                });
            }
            filled += count;
        }
        Ok(BlockRead::Full)
    }

    fn check_no_trailing_data(&mut self) -> Result<(), ArchiveError> {
        // GNU tar pads to a blocking factor with zero blocks, so trailing zeros
        // are legal; trailing *content* is not.
        let mut chunk = [0u8; BLOCK];
        loop {
            let mut filled = 0usize;
            while filled < BLOCK {
                let count = self.source.read(&mut chunk[filled..])?;
                if count == 0 {
                    break;
                }
                filled += count;
            }
            if filled == 0 {
                return Ok(());
            }
            if chunk[..filled].iter().any(|byte| *byte != 0) {
                return Err(ArchiveError::ArchiveInvalid(
                    "non-zero bytes follow the archive end marker".into(),
                ));
            }
            if filled < BLOCK {
                return Ok(());
            }
        }
    }
}

#[derive(Debug, PartialEq, Eq)]
enum BlockRead {
    Full,
    Short(usize),
    Eof,
}

fn padding_for(size: u64) -> usize {
    let remainder = (size % BLOCK as u64) as usize;
    if remainder == 0 { 0 } else { BLOCK - remainder }
}

fn strip_nul(bytes: &[u8]) -> &[u8] {
    match bytes.iter().position(|byte| *byte == 0) {
        Some(index) => &bytes[..index],
        None => bytes,
    }
}

fn header_path(header: &[u8; BLOCK]) -> Result<String, ArchiveError> {
    let name = strip_nul(&header[0..100]);
    let prefix = strip_nul(&header[345..500]);
    let magic = &header[257..262];
    let mut raw = Vec::new();
    if magic == b"ustar" && !prefix.is_empty() {
        raw.extend_from_slice(prefix);
        raw.push(b'/');
    }
    raw.extend_from_slice(name);
    String::from_utf8(raw).map_err(|_| ArchiveError::PathInvalid("<non-utf8 member path>".into()))
}

/// Applies architecture.md 10.4's path rules.
fn validate_path(raw: &str) -> Result<String, ArchiveError> {
    if raw.is_empty() {
        return Err(ArchiveError::PathInvalid(raw.to_string()));
    }
    if raw.len() > MAX_PATH_LEN {
        return Err(ArchiveError::PathInvalid(format!("<{} bytes>", raw.len())));
    }
    if raw.starts_with('/') {
        return Err(ArchiveError::PathAbsolute(raw.to_string()));
    }
    // A Windows-style drive letter or UNC path is absolute too, and a
    // backslash is not a separator here — either way the path is unusable.
    if raw.contains('\\') {
        return Err(ArchiveError::PathInvalid(raw.to_string()));
    }
    if raw.contains('\0') || raw.chars().any(|c| c.is_control()) {
        return Err(ArchiveError::PathInvalid(raw.to_string()));
    }
    let mut depth = 0i64;
    for component in raw.split('/') {
        match component {
            "" | "." => continue,
            ".." => {
                depth -= 1;
                if depth < 0 {
                    return Err(ArchiveError::PathTraversal(raw.to_string()));
                }
            }
            _ => depth += 1,
        }
    }
    // Normalize to the form the manifest records: no leading "./", no
    // redundant separators, so two spellings of one member cannot both pass
    // the duplicate check.
    let normalized: Vec<&str> = raw
        .split('/')
        .filter(|component| !component.is_empty() && *component != ".")
        .collect();
    if normalized.is_empty() {
        return Err(ArchiveError::PathInvalid(raw.to_string()));
    }
    if normalized.contains(&"..") {
        // A `..` that stays within the archive is still a shape a firmware
        // bundle never needs, and normalizing it would make two paths collide.
        return Err(ArchiveError::PathTraversal(raw.to_string()));
    }
    Ok(normalized.join("/"))
}

fn verify_checksum(header: &[u8; BLOCK]) -> Result<(), ArchiveError> {
    let declared = parse_octal(&header[148..156], "checksum")?;
    let mut unsigned_sum = 0u64;
    let mut signed_sum = 0i64;
    for (index, byte) in header.iter().enumerate() {
        let value = if (148..156).contains(&index) {
            b' '
        } else {
            *byte
        };
        unsigned_sum += value as u64;
        signed_sum += (value as i8) as i64;
    }
    if declared == unsigned_sum || declared as i64 == signed_sum {
        Ok(())
    } else {
        Err(ArchiveError::ArchiveInvalid(format!(
            "member header checksum mismatch: declared {declared}, computed {unsigned_sum}"
        )))
    }
}

fn parse_octal(field: &[u8], name: &str) -> Result<u64, ArchiveError> {
    // GNU base-256 encoding for values that do not fit in the octal field.
    if field[0] & 0x80 != 0 {
        let mut value = 0u64;
        for byte in &field[1..] {
            value = value
                .checked_mul(256)
                .and_then(|shifted| shifted.checked_add(*byte as u64))
                .ok_or_else(|| {
                    ArchiveError::ArchiveInvalid(format!("{name} field overflows u64"))
                })?;
        }
        return Ok(value);
    }
    let text: String = field
        .iter()
        .take_while(|byte| **byte != 0 && **byte != b' ')
        .map(|byte| *byte as char)
        .collect();
    let text = text.trim();
    if text.is_empty() {
        return Ok(0);
    }
    u64::from_str_radix(text, 8)
        .map_err(|_| ArchiveError::ArchiveInvalid(format!("{name} field is not octal: {text:?}")))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Builds a ustar archive in memory so the hazard vectors can be produced
    /// exactly, without relying on a tar implementation that would refuse to
    /// emit them.
    struct TarBuilder {
        bytes: Vec<u8>,
    }

    impl TarBuilder {
        fn new() -> Self {
            TarBuilder { bytes: Vec::new() }
        }

        fn add(&mut self, path: &str, type_flag: u8, body: &[u8]) -> &mut Self {
            self.add_with_size(path, type_flag, body, body.len() as u64)
        }

        fn add_with_size(
            &mut self,
            path: &str,
            type_flag: u8,
            body: &[u8],
            declared_size: u64,
        ) -> &mut Self {
            let mut header = [0u8; BLOCK];
            let name = path.as_bytes();
            header[..name.len().min(100)].copy_from_slice(&name[..name.len().min(100)]);
            write_octal(&mut header[100..108], 0o644);
            write_octal(&mut header[108..116], 0);
            write_octal(&mut header[116..124], 0);
            write_octal(&mut header[124..136], declared_size);
            write_octal(&mut header[136..148], 0);
            header[156] = type_flag;
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
            self.bytes
                .extend_from_slice(&vec![0u8; padding_for(body.len() as u64)]);
            self
        }

        fn finish(&mut self) -> Vec<u8> {
            let mut out = self.bytes.clone();
            out.extend_from_slice(&[0u8; BLOCK * 2]);
            out
        }

        /// Deliberately omits the end marker.
        fn finish_without_end_marker(&mut self) -> Vec<u8> {
            self.bytes.clone()
        }
    }

    fn write_octal(field: &mut [u8], value: u64) {
        let text = format!("{:0width$o}", value, width = field.len() - 1);
        field[..text.len()].copy_from_slice(text.as_bytes());
        field[field.len() - 1] = 0;
    }

    fn read_all(bytes: &[u8]) -> Result<Vec<TarMemberObservation>, ArchiveError> {
        let mut reader = TarReader::new(bytes);
        let mut out = Vec::new();
        while let Some(header) = reader.next_member()? {
            out.push(reader.read_member_body(&header, |_| {})?);
        }
        Ok(out)
    }

    #[test]
    fn reads_members_and_hashes_their_bodies() {
        let bytes = TarBuilder::new()
            .add("parameter.txt", b'0', b"CMDLINE:mtdparts=...")
            .add("uboot.img", b'0', &vec![0xAB; 1500])
            .finish();
        let members = read_all(&bytes).unwrap();
        assert_eq!(members.len(), 2);
        assert_eq!(members[0].path, "parameter.txt");
        assert_eq!(members[0].size, 20);
        assert_eq!(
            members[0].sha256,
            arkforge_core::digest::sha256(b"CMDLINE:mtdparts=...")
        );
        assert_eq!(members[1].size, 1500);
    }

    #[test]
    fn hazard_vectors_map_to_the_arkdeck_contract_codes() {
        let cases: Vec<(&str, Vec<u8>, &str)> = vec![
            (
                "arc003-absolute-path",
                TarBuilder::new().add("/etc/passwd", b'0', b"x").finish(),
                "ARC003_PATH_ABSOLUTE",
            ),
            (
                "arc004-traversal",
                TarBuilder::new().add("../outside.img", b'0', b"x").finish(),
                "ARC004_PATH_TRAVERSAL",
            ),
            (
                "arc005-backslash",
                TarBuilder::new().add("dir\\file.img", b'0', b"x").finish(),
                "ARC005_PATH_INVALID",
            ),
            (
                "arc006-duplicate",
                TarBuilder::new()
                    .add("system.img", b'0', b"a")
                    .add("system.img", b'0', b"b")
                    .finish(),
                "ARC006_PATH_DUPLICATE",
            ),
            (
                "arc007-symlink",
                TarBuilder::new().add("link", b'2', b"").finish(),
                "ARC007_LINK_UNSUPPORTED",
            ),
            (
                "arc007-hardlink",
                TarBuilder::new().add("link", b'1', b"").finish(),
                "ARC007_LINK_UNSUPPORTED",
            ),
            (
                "arc008-directory",
                TarBuilder::new().add("subdir/", b'5', b"").finish(),
                "ARC008_MEMBER_TYPE_UNSUPPORTED",
            ),
            (
                "arc008-char-device",
                TarBuilder::new().add("dev", b'3', b"").finish(),
                "ARC008_MEMBER_TYPE_UNSUPPORTED",
            ),
            (
                "arc008-fifo",
                TarBuilder::new().add("pipe", b'6', b"").finish(),
                "ARC008_MEMBER_TYPE_UNSUPPORTED",
            ),
            (
                "arc009-short-body",
                TarBuilder::new()
                    .add_with_size("system.img", b'0', b"only-8-b", 4096)
                    .finish(),
                "ARC009_MEMBER_SIZE_MISMATCH",
            ),
            (
                "arc002-missing-end-marker",
                TarBuilder::new()
                    .add("system.img", b'0', b"x")
                    .finish_without_end_marker(),
                "ARC002_ARCHIVE_INVALID",
            ),
        ];
        for (vector, bytes, expected) in cases {
            let error = read_all(&bytes)
                .err()
                .unwrap_or_else(|| panic!("{vector} should have been rejected"));
            assert_eq!(error.code(), expected, "{vector}: {error}");
        }
    }

    #[test]
    fn trailing_garbage_after_the_end_marker_is_rejected() {
        let mut bytes = TarBuilder::new().add("a.img", b'0', b"x").finish();
        bytes.extend_from_slice(b"garbage that is not padding");
        let error = read_all(&bytes).unwrap_err();
        assert_eq!(error.code(), "ARC002_ARCHIVE_INVALID");
        assert!(error.to_string().contains("follow the archive end marker"));
    }

    #[test]
    fn trailing_zero_padding_after_the_end_marker_is_accepted() {
        // GNU tar pads to a 10 KiB blocking factor by default.
        let mut bytes = TarBuilder::new().add("a.img", b'0', b"x").finish();
        bytes.extend_from_slice(&vec![0u8; BLOCK * 17]);
        assert_eq!(read_all(&bytes).unwrap().len(), 1);
    }

    #[test]
    fn a_corrupted_header_checksum_is_rejected() {
        let mut bytes = TarBuilder::new().add("a.img", b'0', b"x").finish();
        bytes[0] = b'z';
        let error = read_all(&bytes).unwrap_err();
        assert_eq!(error.code(), "ARC002_ARCHIVE_INVALID");
        assert!(error.to_string().contains("checksum"));
    }

    #[test]
    fn leading_dot_slash_normalizes_and_then_collides_with_its_twin() {
        let bytes = TarBuilder::new()
            .add("./system.img", b'0', b"a")
            .add("system.img", b'0', b"b")
            .finish();
        let error = read_all(&bytes).unwrap_err();
        assert_eq!(error.code(), "ARC006_PATH_DUPLICATE");
    }

    #[test]
    fn a_path_that_only_dips_into_a_parent_is_still_traversal() {
        let bytes = TarBuilder::new()
            .add("a/../system.img", b'0', b"x")
            .finish();
        assert_eq!(
            read_all(&bytes).unwrap_err().code(),
            "ARC004_PATH_TRAVERSAL"
        );
    }

    /// The pinned DAYU200 archive is flat — 17 regular members, no directory
    /// entries (ArkDeck `member-inventory.json`). The fixture is built the same
    /// way, because a directory member is an ARC008 rejection by contract.
    #[test]
    fn interoperates_with_system_tar() {
        use std::process::Command;
        let dir = std::env::temp_dir().join(format!("arkforge-tar-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("parameter.txt"), b"CMDLINE:mtdparts=x").unwrap();
        std::fs::write(dir.join("uboot.img"), vec![0x5Au8; 9000]).unwrap();
        let archive = dir.join("images.tar");
        let status = Command::new("tar")
            .arg("-cf")
            .arg(&archive)
            .arg("-C")
            .arg(&dir)
            .arg("parameter.txt")
            .arg("uboot.img")
            .status();
        let Ok(status) = status else {
            let _ = std::fs::remove_dir_all(&dir);
            return;
        };
        assert!(status.success());
        let bytes = std::fs::read(&archive).unwrap();
        let members = read_all(&bytes).unwrap_or_else(|error| {
            let _ = std::fs::remove_dir_all(&dir);
            panic!("system tar archive rejected: {error}")
        });
        let paths: Vec<&str> = members.iter().map(|m| m.path.as_str()).collect();
        assert!(paths.contains(&"parameter.txt"), "{paths:?}");
        assert!(paths.contains(&"uboot.img"), "{paths:?}");
        let uboot = members.iter().find(|m| m.path == "uboot.img").unwrap();
        assert_eq!(uboot.size, 9000);
        assert_eq!(
            uboot.sha256,
            arkforge_core::digest::sha256(&vec![0x5Au8; 9000])
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// A directory member is rejected even when a well-behaved tar produced it,
    /// which is the ArkDeck `arc008-directory` vector.
    #[test]
    fn a_system_tar_archive_containing_a_directory_is_rejected() {
        use std::process::Command;
        let dir = std::env::temp_dir().join(format!("arkforge-tar-dir-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(dir.join("images")).unwrap();
        std::fs::write(dir.join("images/parameter.txt"), b"x").unwrap();
        let archive = dir.join("images.tar");
        let status = Command::new("tar")
            .arg("-cf")
            .arg(&archive)
            .arg("-C")
            .arg(&dir)
            .arg("images")
            .status();
        let Ok(status) = status else {
            let _ = std::fs::remove_dir_all(&dir);
            return;
        };
        assert!(status.success());
        let bytes = std::fs::read(&archive).unwrap();
        let error = read_all(&bytes).unwrap_err();
        assert_eq!(error.code(), "ARC008_MEMBER_TYPE_UNSUPPORTED");
        let _ = std::fs::remove_dir_all(&dir);
    }
}
