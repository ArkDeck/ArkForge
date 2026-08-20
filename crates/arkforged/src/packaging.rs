//! The macOS signing / entitlement / packaging contract (AD-007).
//!
//! architecture.md 21.2 Stage B treats signing as an explicit design item.
//! The reader validates the `arkforged` artifact used by packaging and release
//! tooling; the Runtime itself has no child executable to validate.
//!
//! # What this module enforces
//!
//! Two lessons, each already paid for once, each a different layer of "the
//! bytes will not run":
//!
//! - **AD-007** — a non-empty entitlement dictionary on `arkforged`. Refused in
//!   every mode because the shipped daemon contract is an empty dictionary.
//! - **AD-015 / AD-011** — Developer ID signature, Hardened Runtime, secure
//!   timestamp. Refused only in [`ContractMode::Release`] so local development
//!   builds can still be inspected.
//!
//! # What this module deliberately does not do
//!
//! **It does not check artifact identity.** The caller supplies the exact file;
//! the package manifest and ArkDeck installer pin its digest separately. This
//! module answers only whether its Mach-O signing shape satisfies release.
//!
//! **It does not attest this process to itself.** `arkdeck-agentd` verifies
//! `arkforged` at install time and re-verifies its file identity before spawn.
//!
//! **It does not assess Gatekeeper.** Notarization and stapling are verified
//! once by the packager (`spctl --assess`), not on every daemon start: an
//! assessment can reach the network, and daemon startup is not the place for
//! that. See `packaging/macos/package-arkforged.sh`.
//!
//! The full contract is
//! `docs/decisions/AFD-0003-arkforged-signing-packaging.md`.

use std::fmt;
use std::path::Path;

/// Where the prose contract lives, so a refusal can point at it.
pub const CONTRACT_DOC: &str = "docs/decisions/AFD-0003-arkforged-signing-packaging.md";

/// The entitlement dictionary this repository ships for the native daemon.
/// Compiled in so a test can assert the release input still says what the
/// contract says.
pub const ARKFORGED_ENTITLEMENTS: &str =
    include_str!("../../../packaging/macos/arkforged.entitlements");

/// Keys that must never appear, and what each one would actually do here.
///
/// The contract is "the dictionary is empty", which is stronger than any
/// denylist and is what the check enforces. This table exists for the sentence
/// a refusal prints: an operator who sees `app-sandbox` deserves to be told it
/// aborts before `main` rather than left to discover it from a crash report.
pub const FORBIDDEN_ENTITLEMENTS: &[(&str, &str)] = &[
    (
        "com.apple.security.app-sandbox",
        "declares App Sandbox; the parent process is not sandboxed, so libsecinit aborts this \
         binary before main (AD-007)",
    ),
    (
        "com.apple.security.inherit",
        "asks to inherit a sandbox profile the parent does not have; same abort before main \
         (AD-007)",
    ),
    (
        "com.apple.security.get-task-allow",
        "lets any process running as this user attach a debugger to a process that writes \
         partitions",
    ),
    (
        "com.apple.security.cs.disable-library-validation",
        "lets code nobody in this contract signed load into the process",
    ),
    (
        "com.apple.security.cs.allow-dyld-environment-variables",
        "lets the caller's environment choose which libraries load, which is the caller \
         choosing what runs",
    ),
    (
        "com.apple.security.cs.allow-unsigned-executable-memory",
        "removes the guarantee that what executes is what was signed",
    ),
    (
        "com.apple.security.cs.allow-jit",
        "removes the guarantee that what executes is what was signed",
    ),
    (
        "com.apple.security.cs.disable-executable-page-protection",
        "removes the guarantee that what executes is what was signed",
    ),
    (
        "com.apple.security.cs.debugger",
        "makes this binary a debugger for others on the host",
    ),
    (
        "keychain-access-groups",
        "claims secrets at rest; arkforged holds none — the pairing secret arrives on stdin and \
         never leaves memory (architecture.md 15.2)",
    ),
];

/// How strictly the contract applies to a binary.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ContractMode {
    /// A local build. The entitlement clause still applies; the signing clause
    /// is reported rather than enforced.
    Development,
    /// A shipped build. Everything applies.
    Release,
}

/// A Mach-O file's signing facts, one entry per architecture slice.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SignedCode {
    pub slices: Vec<Slice>,
}

/// One architecture slice and whatever signature it carries.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Slice {
    pub architecture: Architecture,
    pub signature: Option<Signature>,
}

/// The subset of a code signature this contract has an opinion about.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Signature {
    pub identifier: String,
    pub team_identifier: Option<String>,
    pub ad_hoc: bool,
    /// Signed by the linker rather than by `codesign`. Always ad-hoc in
    /// practice, and reported separately because it says *how* the binary came
    /// to be unsigned: nobody ran `codesign` at all.
    pub linker_signed: bool,
    pub hardened_runtime: bool,
    /// Entitlement keys. Empty means an empty dictionary or no dictionary —
    /// the two the contract treats alike, as ArkDeck's validator does.
    pub entitlements: Vec<String>,
    pub entitlement_encoding: Option<EntitlementEncoding>,
}

/// Which slot the entitlement keys were read from.
///
/// `codesign` emits both for a modern signature. Recorded because a binary
/// carrying only the DER slot is read by a different code path here, and a
/// diagnosis should say which one answered.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EntitlementEncoding {
    Xml,
    Der,
}

/// A CPU type, named where the name is well known.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Architecture(pub u32);

impl fmt::Display for Architecture {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self.0 {
            0x0100_000c => f.write_str("arm64"),
            0x0100_0007 => f.write_str("x86_64"),
            0x0000_000c => f.write_str("arm"),
            0x0000_0007 => f.write_str("i386"),
            other => write!(f, "cputype 0x{other:08x}"),
        }
    }
}

/// Why a file could not be read as signed Mach-O code.
///
/// Every rejection is typed and none of them panics: this parses a file an
/// operator named on a command line, and a daemon that holds device authority
/// must not have a parser that aborts it (architecture.md 20.1).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ReadError {
    NotMachO,
    /// Big-endian Mach-O (PowerPC). Refused rather than misread — saying
    /// nothing about a binary is not the same as saying it is clean.
    UnsupportedByteOrder,
    TooLarge(u64),
    Truncated(&'static str),
    Malformed(&'static str),
    Io(String),
}

impl fmt::Display for ReadError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            ReadError::NotMachO => f.write_str("not a Mach-O file"),
            ReadError::UnsupportedByteOrder => {
                f.write_str("big-endian Mach-O, which this reader does not decode")
            }
            ReadError::TooLarge(size) => write!(
                f,
                "{size} bytes is past the {} MiB this reader will hold in memory",
                MAX_READ_BYTES / (1 << 20)
            ),
            ReadError::Truncated(what) => write!(f, "truncated: {what}"),
            ReadError::Malformed(what) => write!(f, "malformed: {what}"),
            ReadError::Io(message) => f.write_str(message),
        }
    }
}

/// Why a binary does not meet the contract.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ContractViolation {
    /// The AD-007 clause. Applies in every mode.
    EntitlementsNotEmpty {
        architecture: Architecture,
        keys: Vec<String>,
    },
    Unsigned {
        architecture: Architecture,
    },
    AdHocSignature {
        architecture: Architecture,
        linker_signed: bool,
    },
    NoHardenedRuntime {
        architecture: Architecture,
    },
    NoTeamIdentifier {
        architecture: Architecture,
    },
}

impl ContractViolation {
    /// A stable code, so a caller can branch without parsing prose.
    pub fn code(&self) -> &'static str {
        match self {
            ContractViolation::EntitlementsNotEmpty { .. } => "ENTITLEMENTS_NOT_EMPTY",
            ContractViolation::Unsigned { .. } => "UNSIGNED",
            ContractViolation::AdHocSignature { .. } => "ADHOC_SIGNATURE",
            ContractViolation::NoHardenedRuntime { .. } => "NO_HARDENED_RUNTIME",
            ContractViolation::NoTeamIdentifier { .. } => "NO_TEAM_IDENTIFIER",
        }
    }
}

impl fmt::Display for ContractViolation {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            ContractViolation::EntitlementsNotEmpty { architecture, keys } => {
                write!(
                    f,
                    "the {architecture} slice declares {} entitlement(s) where the contract \
                     requires an empty dictionary",
                    keys.len()
                )?;
                for key in keys {
                    match FORBIDDEN_ENTITLEMENTS.iter().find(|(name, _)| name == key) {
                        Some((_, why)) => write!(f, "\n    {key}: {why}")?,
                        None => write!(f, "\n    {key}")?,
                    }
                }
                Ok(())
            }
            ContractViolation::Unsigned { architecture } => write!(
                f,
                "the {architecture} slice carries no code signature; unsigned bytes that pick up \
                 com.apple.quarantine hang in dyld with a perfect digest (AD-011/AD-015)"
            ),
            ContractViolation::AdHocSignature {
                architecture,
                linker_signed,
            } => write!(
                f,
                "the {architecture} slice is ad-hoc signed{}; an ad-hoc signature cannot be \
                 notarized, and un-notarized bytes are the quarantine hang waiting to happen \
                 (AD-011/AD-015)",
                if *linker_signed {
                    " by the linker, which means nobody ran codesign on it"
                } else {
                    ""
                }
            ),
            ContractViolation::NoHardenedRuntime { architecture } => write!(
                f,
                "the {architecture} slice was not signed with --options runtime; notarization \
                 requires Hardened Runtime"
            ),
            ContractViolation::NoTeamIdentifier { architecture } => write!(
                f,
                "the {architecture} slice carries no Team ID, so it was not signed with a \
                 Developer ID certificate"
            ),
        }
    }
}

impl SignedCode {
    /// Every way this binary departs from the contract, in the given mode.
    ///
    /// Every slice is checked rather than the one this host would run — the
    /// same reason ArkDeck passes `kSecCSCheckAllArchitectures`. A fat binary
    /// whose second slice carries an App Sandbox key is a binary that fails on
    /// somebody else's machine.
    pub fn violations(&self, mode: ContractMode) -> Vec<ContractViolation> {
        let mut out = Vec::new();
        for slice in &self.slices {
            let architecture = slice.architecture;
            let Some(signature) = &slice.signature else {
                if mode == ContractMode::Release {
                    out.push(ContractViolation::Unsigned { architecture });
                }
                continue;
            };
            if !signature.entitlements.is_empty() {
                out.push(ContractViolation::EntitlementsNotEmpty {
                    architecture,
                    keys: signature.entitlements.clone(),
                });
            }
            if mode == ContractMode::Development {
                continue;
            }
            if signature.ad_hoc || signature.linker_signed {
                out.push(ContractViolation::AdHocSignature {
                    architecture,
                    linker_signed: signature.linker_signed,
                });
            }
            if !signature.hardened_runtime {
                out.push(ContractViolation::NoHardenedRuntime { architecture });
            }
            if signature.team_identifier.is_none() {
                out.push(ContractViolation::NoTeamIdentifier { architecture });
            }
        }
        out
    }

    /// One line per slice, for the startup log. What was observed, not a verdict.
    pub fn summary(&self) -> String {
        self.slices
            .iter()
            .map(|slice| {
                let Some(signature) = &slice.signature else {
                    return format!("{} unsigned", slice.architecture);
                };
                let mut notes = Vec::new();
                if signature.linker_signed {
                    notes.push("linker-signed".to_string());
                } else if signature.ad_hoc {
                    notes.push("adhoc".to_string());
                }
                if signature.hardened_runtime {
                    notes.push("runtime".to_string());
                }
                match &signature.team_identifier {
                    Some(team) => notes.push(format!("team {team}")),
                    None => notes.push("no team".to_string()),
                }
                notes.push(if signature.entitlements.is_empty() {
                    "no entitlements".to_string()
                } else {
                    format!("{} entitlements", signature.entitlements.len())
                });
                format!(
                    "{} {} ({})",
                    slice.architecture,
                    signature.identifier,
                    notes.join(", ")
                )
            })
            .collect::<Vec<_>>()
            .join("; ")
    }
}

/// A signed code artifact larger than this is its own red flag; refusing to
/// buffer it is cheaper than discovering the memory ceiling in production.
const MAX_READ_BYTES: u64 = 64 << 20;

/// Reads a file's signing facts.
pub fn read_file(path: &Path) -> Result<SignedCode, ReadError> {
    let size = std::fs::metadata(path)
        .map_err(|error| ReadError::Io(format!("{}: {error}", path.display())))?
        .len();
    if size > MAX_READ_BYTES {
        return Err(ReadError::TooLarge(size));
    }
    let bytes = std::fs::read(path)
        .map_err(|error| ReadError::Io(format!("{}: {error}", path.display())))?;
    read(&bytes)
}

/// Reads signing facts from Mach-O bytes, thin or universal.
pub fn read(bytes: &[u8]) -> Result<SignedCode, ReadError> {
    match be32(bytes, 0).ok_or(ReadError::NotMachO)? {
        FAT_MAGIC | FAT_MAGIC_64 => read_universal(bytes),
        _ => Ok(SignedCode {
            slices: vec![read_thin(bytes, 0, bytes.len())?],
        }),
    }
}

const FAT_MAGIC: u32 = 0xcafe_babe;
const FAT_MAGIC_64: u32 = 0xcafe_babf;
const MH_MAGIC_64: u32 = 0xfeed_facf;
const MH_MAGIC_32: u32 = 0xfeed_face;
const MH_CIGAM_64: u32 = 0xcffa_edfe;
const MH_CIGAM_32: u32 = 0xcefa_edfe;
const LC_CODE_SIGNATURE: u32 = 0x1d;
const CSMAGIC_EMBEDDED_SIGNATURE: u32 = 0xfade_0cc0;
const CSMAGIC_CODEDIRECTORY: u32 = 0xfade_0c02;
const CSMAGIC_ENTITLEMENTS: u32 = 0xfade_7171;
const CSMAGIC_DER_ENTITLEMENTS: u32 = 0xfade_7172;
const CSSLOT_ENTITLEMENTS: u32 = 5;
const CSSLOT_DER_ENTITLEMENTS: u32 = 7;
const CS_ADHOC: u32 = 0x0000_0002;
const CS_RUNTIME: u32 = 0x0001_0000;
const CS_LINKER_SIGNED: u32 = 0x0002_0000;
/// Enough slices for any real universal binary; past it the header is lying.
const MAX_SLICES: u32 = 64;
/// Enough load commands for any real Mach-O; past it the header is lying.
const MAX_LOAD_COMMANDS: u32 = 4096;

fn read_universal(bytes: &[u8]) -> Result<SignedCode, ReadError> {
    let wide = be32(bytes, 0) == Some(FAT_MAGIC_64);
    let count = be32(bytes, 4).ok_or(ReadError::Truncated("fat header"))?;
    if count > MAX_SLICES {
        return Err(ReadError::Malformed("fat header claims too many slices"));
    }
    let stride = if wide { 32 } else { 20 };
    let mut slices = Vec::new();
    for index in 0..count as usize {
        let entry = 8 + index * stride;
        let cpu_type = be32(bytes, entry).ok_or(ReadError::Truncated("fat arch entry"))?;
        let (offset, size) = if wide {
            (
                be64(bytes, entry + 8).ok_or(ReadError::Truncated("fat arch entry"))? as usize,
                be64(bytes, entry + 16).ok_or(ReadError::Truncated("fat arch entry"))? as usize,
            )
        } else {
            (
                be32(bytes, entry + 8).ok_or(ReadError::Truncated("fat arch entry"))? as usize,
                be32(bytes, entry + 12).ok_or(ReadError::Truncated("fat arch entry"))? as usize,
            )
        };
        let end = offset
            .checked_add(size)
            .ok_or(ReadError::Malformed("fat slice extends past usize"))?;
        if end > bytes.len() {
            return Err(ReadError::Truncated("fat slice extends past the file"));
        }
        let mut slice = read_thin(bytes, offset, end)?;
        // The fat header names the architecture even when the slice's own
        // header disagrees; trust the slice, fall back to the header.
        if slice.architecture.0 == 0 {
            slice.architecture = Architecture(cpu_type);
        }
        slices.push(slice);
    }
    Ok(SignedCode { slices })
}

fn read_thin(bytes: &[u8], start: usize, end: usize) -> Result<Slice, ReadError> {
    let image = bytes.get(start..end).ok_or(ReadError::Truncated("slice"))?;
    let magic = le32(image, 0).ok_or(ReadError::NotMachO)?;
    let header_size = match magic {
        MH_MAGIC_64 => 32,
        MH_MAGIC_32 => 28,
        MH_CIGAM_64 | MH_CIGAM_32 => return Err(ReadError::UnsupportedByteOrder),
        _ => return Err(ReadError::NotMachO),
    };
    let cpu_type = le32(image, 4).ok_or(ReadError::Truncated("mach header"))?;
    let command_count = le32(image, 16).ok_or(ReadError::Truncated("mach header"))?;
    if command_count > MAX_LOAD_COMMANDS {
        return Err(ReadError::Malformed(
            "mach header claims too many load commands",
        ));
    }

    let mut cursor = header_size;
    let mut signature_region = None;
    for _ in 0..command_count {
        let command = le32(image, cursor).ok_or(ReadError::Truncated("load command"))?;
        let size = le32(image, cursor + 4).ok_or(ReadError::Truncated("load command"))? as usize;
        // A zero-size command would loop forever, and an unaligned one is not a
        // command; either way the header is not describing this file.
        if size < 8 || !size.is_multiple_of(4) {
            return Err(ReadError::Malformed("load command size"));
        }
        if command == LC_CODE_SIGNATURE {
            let offset =
                le32(image, cursor + 8).ok_or(ReadError::Truncated("LC_CODE_SIGNATURE"))?;
            let length =
                le32(image, cursor + 12).ok_or(ReadError::Truncated("LC_CODE_SIGNATURE"))?;
            signature_region = Some((offset as usize, length as usize));
            break;
        }
        cursor = cursor
            .checked_add(size)
            .ok_or(ReadError::Malformed("load command table wraps"))?;
    }

    let architecture = Architecture(cpu_type);
    let Some((offset, length)) = signature_region else {
        return Ok(Slice {
            architecture,
            signature: None,
        });
    };
    let region_end = offset
        .checked_add(length)
        .ok_or(ReadError::Malformed("signature region wraps"))?;
    let region = image
        .get(offset..region_end)
        .ok_or(ReadError::Truncated("code signature region"))?;
    Ok(Slice {
        architecture,
        signature: Some(read_signature(region)?),
    })
}

fn read_signature(region: &[u8]) -> Result<Signature, ReadError> {
    if be32(region, 0) != Some(CSMAGIC_EMBEDDED_SIGNATURE) {
        return Err(ReadError::Malformed(
            "code signature is not an embedded SuperBlob",
        ));
    }
    let count = be32(region, 8).ok_or(ReadError::Truncated("SuperBlob header"))?;
    if count > MAX_SLICES * 8 {
        return Err(ReadError::Malformed("SuperBlob claims too many blobs"));
    }

    let mut directory = None;
    let mut xml_entitlements = None;
    let mut der_entitlements = None;
    for index in 0..count as usize {
        let entry = 12 + index * 8;
        let slot = be32(region, entry).ok_or(ReadError::Truncated("SuperBlob index"))?;
        let offset =
            be32(region, entry + 4).ok_or(ReadError::Truncated("SuperBlob index"))? as usize;
        let magic = be32(region, offset).ok_or(ReadError::Truncated("blob header"))?;
        let length = be32(region, offset + 4).ok_or(ReadError::Truncated("blob header"))? as usize;
        if length < 8 {
            return Err(ReadError::Malformed("blob length"));
        }
        let end = offset
            .checked_add(length)
            .ok_or(ReadError::Malformed("blob extends past usize"))?;
        let body = region
            .get(offset + 8..end)
            .ok_or(ReadError::Truncated("blob body"))?;
        match (slot, magic) {
            (0, CSMAGIC_CODEDIRECTORY) => {
                directory = Some(
                    region
                        .get(offset..end)
                        .ok_or(ReadError::Truncated("CodeDirectory"))?,
                )
            }
            (CSSLOT_ENTITLEMENTS, CSMAGIC_ENTITLEMENTS) => xml_entitlements = Some(body),
            (CSSLOT_DER_ENTITLEMENTS, CSMAGIC_DER_ENTITLEMENTS) => der_entitlements = Some(body),
            _ => {}
        }
    }

    let directory = directory.ok_or(ReadError::Malformed("no CodeDirectory in the SuperBlob"))?;
    let version = be32(directory, 8).ok_or(ReadError::Truncated("CodeDirectory"))?;
    let flags = be32(directory, 12).ok_or(ReadError::Truncated("CodeDirectory"))?;
    let identifier_offset =
        be32(directory, 20).ok_or(ReadError::Truncated("CodeDirectory"))? as usize;
    // `teamOffset` only exists from version 0x20200; reading it from an older
    // directory would read whatever follows.
    let team_identifier = if version >= 0x0002_0200 {
        match be32(directory, 48).ok_or(ReadError::Truncated("CodeDirectory"))? {
            0 => None,
            offset => Some(c_string(directory, offset as usize)?),
        }
    } else {
        None
    };

    let (entitlements, entitlement_encoding) = match (xml_entitlements, der_entitlements) {
        (Some(xml), _) => (
            plist_keys(&String::from_utf8_lossy(xml)),
            Some(EntitlementEncoding::Xml),
        ),
        (None, Some(der)) => (der_keys(der), Some(EntitlementEncoding::Der)),
        (None, None) => (Vec::new(), None),
    };

    Ok(Signature {
        identifier: c_string(directory, identifier_offset)?,
        team_identifier,
        ad_hoc: flags & CS_ADHOC != 0,
        linker_signed: flags & CS_LINKER_SIGNED != 0,
        hardened_runtime: flags & CS_RUNTIME != 0,
        entitlements,
        entitlement_encoding,
    })
}

/// The `<key>` names in an entitlements plist.
///
/// Deliberately a scan and not a plist parser. The contract is "no keys at
/// all", so a scan that over-collects — a nested dictionary's keys, say — errs
/// toward refusing, which is the safe direction. A real plist parser here
/// would be a second XML implementation to keep correct for no gain.
pub fn plist_keys(xml: &str) -> Vec<String> {
    let mut keys = Vec::new();
    let mut rest = xml;
    while let Some(open) = rest.find("<key>") {
        let after = &rest[open + 5..];
        let Some(close) = after.find("</key>") else {
            break;
        };
        keys.push(after[..close].trim().to_string());
        rest = &after[close + 6..];
    }
    keys
}

/// Key names from the DER entitlements slot.
///
/// Apple's shape, read off a real signature on 2026-08-16: an
/// application-tagged constructed value holding an INTEGER version and a
/// constructed container of `SEQUENCE { UTF8String key, value }`. Rather than
/// hard-code that nesting, this walks any constructed value and takes the first
/// UTF8String of every SEQUENCE — same over-collecting bias as the XML scan,
/// and it does not break if Apple adds a level.
fn der_keys(der: &[u8]) -> Vec<String> {
    let mut keys = Vec::new();
    walk_der(der, 0, &mut keys);
    keys
}

fn walk_der(der: &[u8], depth: usize, keys: &mut Vec<String>) {
    // Constructed DER can nest arbitrarily; a bounded walk cannot be made to
    // recurse a stack overflow out of a malformed file.
    if depth > 16 {
        return;
    }
    let mut cursor = 0usize;
    let mut sequence_head = true;
    while cursor < der.len() {
        let tag = der[cursor];
        let Some((body_start, length)) = der_length(der, cursor + 1) else {
            return;
        };
        let Some(body) = der.get(body_start..body_start.saturating_add(length)) else {
            return;
        };
        // 0x0c is UTF8String. Taking only the first element of a SEQUENCE keeps
        // string *values* out of the key list where the shape is the expected
        // one, without depending on it.
        if tag == 0x0c && sequence_head && depth > 0 {
            keys.push(String::from_utf8_lossy(body).to_string());
        } else if tag & 0x20 != 0 {
            walk_der(body, depth + 1, keys);
        }
        sequence_head = false;
        let Some(next) = body_start.checked_add(length) else {
            return;
        };
        if next <= cursor {
            return;
        }
        cursor = next;
    }
}

/// Returns where a DER value's body starts and how long it is.
fn der_length(der: &[u8], at: usize) -> Option<(usize, usize)> {
    let first = *der.get(at)?;
    if first < 0x80 {
        return Some((at + 1, first as usize));
    }
    let count = (first & 0x7f) as usize;
    // 0x80 is the indefinite form, which DER forbids; more than 8 length bytes
    // is a length no file this size can have.
    if count == 0 || count > 8 {
        return None;
    }
    let mut length = 0usize;
    for index in 0..count {
        length = length.checked_shl(8)? | *der.get(at + 1 + index)? as usize;
    }
    Some((at + 1 + count, length))
}

fn c_string(blob: &[u8], offset: usize) -> Result<String, ReadError> {
    let tail = blob
        .get(offset..)
        .ok_or(ReadError::Truncated("string offset past the blob"))?;
    let end = tail
        .iter()
        .position(|byte| *byte == 0)
        .ok_or(ReadError::Malformed("unterminated string"))?;
    Ok(String::from_utf8_lossy(&tail[..end]).to_string())
}

fn le32(bytes: &[u8], at: usize) -> Option<u32> {
    Some(u32::from_le_bytes(bytes.get(at..at + 4)?.try_into().ok()?))
}

fn be32(bytes: &[u8], at: usize) -> Option<u32> {
    Some(u32::from_be_bytes(bytes.get(at..at + 4)?.try_into().ok()?))
}

fn be64(bytes: &[u8], at: usize) -> Option<u64> {
    Some(u64::from_be_bytes(bytes.get(at..at + 8)?.try_into().ok()?))
}
