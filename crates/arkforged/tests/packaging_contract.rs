//! The macOS signing/entitlement/packaging contract, checked rather than
//! written down (AD-007, `docs/decisions/AFD-0003-arkforged-signing-packaging.md`).
//!
//! Two kinds of assertion live here:
//!
//! - the **contract** ones, which fix the shapes the daemon must refuse. The
//!   one that matters most is that a binary declaring App Sandbox is refused in
//!   every mode, because that is the exact shape AD-007 was: a validator asking
//!   for something that cannot run;
//! - the **parser** ones, which fix the reader against real Mach-O layouts and
//!   assert that no input reaches a panic. The daemon holds device authority,
//!   so a parser that aborts on a malformed file is a way to stop it
//!   (architecture.md 20.1).
//!
//! The fixtures are built here rather than committed as binaries. A committed
//! Mach-O is a blob nobody rereads; a builder states the layout in the same
//! file as the assertions, and was checked against three real binaries on
//! 2026-08-16 (see the decision record).

use arkforged::packaging::{
    self, Architecture, ContractMode, ContractViolation, EntitlementEncoding, ReadError,
};

// ---------------------------------------------------------------- the contract

#[test]
fn the_shipped_entitlement_files_are_empty_dictionaries() {
    // The release inputs and the rule are the same statement, so they cannot
    // drift apart: a key added to either file fails here.
    {
        let (name, source) = ("arkforged.entitlements", packaging::ARKFORGED_ENTITLEMENTS);
        let keys = packaging::plist_keys(source);
        assert!(
            keys.is_empty(),
            "packaging/macos/{name} declares {keys:?}; the contract is an empty dictionary"
        );
        assert!(
            source.contains("<dict/>"),
            "packaging/macos/{name} must carry the empty dictionary literally"
        );
    }
}

#[test]
fn the_release_packager_contains_only_the_canonical_cli_daemon_pair() {
    const SCRIPT: &str = include_str!("../../../packaging/macos/package-arkforge.sh");

    assert!(SCRIPT.contains("cp \"$release_bin/arkforge\" \"$stage/arkforge\""));
    assert!(SCRIPT.contains("cp \"$release_bin/arkforged\" \"$stage/arkforged\""));
    assert!(SCRIPT.contains("\\\"arkforge\\\": {"));
    assert!(SCRIPT.contains("\\\"arkforged\\\": {"));
    assert!(SCRIPT.contains("for component in arkforge arkforged"));
    assert!(SCRIPT.contains("-p arkforged --bin arkforged -p arkforge-cli --bin arkforge"));
    assert!(SCRIPT.contains("\"$release_bin/arkforge\" signing verify"));
    assert!(!SCRIPT.contains("arkforge-signing"));
    for retired in ["rkdeveloptool", "RKDEVELOPTOOL"] {
        assert!(
            !SCRIPT.contains(retired),
            "the release packager still carries retired vendor surface {retired}"
        );
    }
}

#[test]
fn an_app_sandbox_entitlement_is_refused_in_every_mode() {
    // AD-007 itself. The shape ArkDeck's validator used to *require* is the
    // shape this refuses, and it refuses it in development too — there is no
    // build in which a sandboxed child of a non-sandboxed parent reaches main.
    let binary = macho(Signed {
        entitlements: Some(Entitlements::Xml(
            "<plist><dict><key>com.apple.security.app-sandbox</key><true/>\
             <key>com.apple.security.inherit</key><true/></dict></plist>",
        )),
        ..developer_id()
    });
    let code = packaging::read(&binary).expect("a signed fixture reads");

    for mode in [ContractMode::Development, ContractMode::Release] {
        let violations = code.violations(mode);
        assert_eq!(
            violations.len(),
            1,
            "{mode:?} should refuse exactly the entitlements: {violations:?}"
        );
        assert_eq!(violations[0].code(), "ENTITLEMENTS_NOT_EMPTY");
    }

    // And the refusal has to say what the key does, not just that it is there.
    let rendered = code.violations(ContractMode::Development)[0].to_string();
    assert!(
        rendered.contains("com.apple.security.app-sandbox"),
        "{rendered}"
    );
    assert!(rendered.contains("before main"), "{rendered}");
    assert!(rendered.contains("AD-007"), "{rendered}");
}

#[test]
fn an_adhoc_local_build_passes_development_and_fails_release() {
    // Local arkforged builds may be ad-hoc and linker-signed. Development
    // accepts that shape while release still requires the full contract.
    let binary = macho(Signed {
        cpu_type: 0x0100_000c,
        identifier: "arkforged.local",
        team: None,
        ad_hoc: true,
        linker_signed: true,
        hardened_runtime: false,
        entitlements: None,
    });
    let code = packaging::read(&binary).expect("a signed fixture reads");

    assert!(
        code.violations(ContractMode::Development).is_empty(),
        "a clean local build must not be refused: {:?}",
        code.violations(ContractMode::Development)
    );

    let release: Vec<&'static str> = code
        .violations(ContractMode::Release)
        .iter()
        .map(ContractViolation::code)
        .collect();
    assert_eq!(
        release,
        vec![
            "ADHOC_SIGNATURE",
            "NO_HARDENED_RUNTIME",
            "NO_TEAM_IDENTIFIER"
        ],
        "release names every reason separately rather than one 'not release' verdict"
    );
}

#[test]
fn the_shipped_arkdeck_component_shape_passes_release() {
    // This is the shape measured on the real ArkDeck.app component on
    // 2026-08-16: Developer ID, Team 8AQTYW5FKR, Hardened Runtime, and an
    // empty entitlement dictionary in both slots. It is the target the
    // packager has to reproduce, so it is asserted as passing.
    let binary = macho(developer_id());
    let code = packaging::read(&binary).expect("a signed fixture reads");
    assert!(
        code.violations(ContractMode::Release).is_empty(),
        "{:?}",
        code.violations(ContractMode::Release)
    );

    let signature = code.slices[0].signature.as_ref().expect("signed");
    assert_eq!(signature.team_identifier.as_deref(), Some("8AQTYW5FKR"));
    assert!(signature.hardened_runtime);
    assert!(!signature.ad_hoc);
    assert!(signature.entitlements.is_empty());
    assert_eq!(
        signature.entitlement_encoding,
        Some(EntitlementEncoding::Xml)
    );
}

#[test]
fn an_unsigned_binary_is_refused_only_by_release() {
    let binary = macho_unsigned();
    let code = packaging::read(&binary).expect("an unsigned Mach-O still reads");
    assert!(code.slices[0].signature.is_none());
    assert!(code.violations(ContractMode::Development).is_empty());
    assert_eq!(
        code.violations(ContractMode::Release)
            .iter()
            .map(ContractViolation::code)
            .collect::<Vec<_>>(),
        vec!["UNSIGNED"]
    );
}

#[test]
fn every_slice_of_a_universal_binary_is_checked() {
    // ArkDeck passes kSecCSCheckAllArchitectures for the same reason: a fat
    // binary whose second slice carries an App Sandbox key is a binary that
    // fails on somebody else's machine, not on the packager's.
    let arm = macho(developer_id());
    let intel = macho(Signed {
        cpu_type: 0x0100_0007,
        entitlements: Some(Entitlements::Xml(
            "<plist><dict><key>com.apple.security.get-task-allow</key><true/></dict></plist>",
        )),
        ..developer_id()
    });
    let code = packaging::read(&fat(&[(0x0100_000c, arm), (0x0100_0007, intel)]))
        .expect("a universal fixture reads");

    assert_eq!(code.slices.len(), 2);
    let violations = code.violations(ContractMode::Release);
    assert_eq!(violations.len(), 1, "{violations:?}");
    match &violations[0] {
        ContractViolation::EntitlementsNotEmpty { architecture, keys } => {
            assert_eq!(architecture.to_string(), "x86_64");
            assert_eq!(keys, &["com.apple.security.get-task-allow"]);
        }
        other => panic!("expected the x86_64 slice to be named: {other:?}"),
    }
}

#[test]
fn der_only_entitlements_are_read_rather_than_missed() {
    // A binary that carries only the DER slot must not read as "no
    // entitlements". Fail-closed matters more here than anywhere: the failure
    // mode is a refused shape passing as a clean one.
    //
    // The bytes are Apple's, taken from /usr/bin/codesign on 2026-08-16:
    //   70 3c 02 01 01 b0 37 30 35 0c 30 <48-byte key> 01 01 ff
    let mut der = vec![
        0x70, 0x3c, 0x02, 0x01, 0x01, 0xb0, 0x37, 0x30, 0x35, 0x0c, 0x30,
    ];
    der.extend_from_slice(b"com.apple.private.codesignkit.signer-source-host");
    der.extend_from_slice(&[0x01, 0x01, 0xff]);
    assert_eq!(
        der.len(),
        62,
        "the lengths in the real blob describe these bytes"
    );

    let binary = macho(Signed {
        entitlements: Some(Entitlements::Der(der)),
        ..developer_id()
    });
    let code = packaging::read(&binary).expect("a signed fixture reads");
    let signature = code.slices[0].signature.as_ref().expect("signed");
    assert_eq!(
        signature.entitlement_encoding,
        Some(EntitlementEncoding::Der)
    );
    assert_eq!(
        signature.entitlements,
        vec!["com.apple.private.codesignkit.signer-source-host"]
    );
    assert_eq!(
        code.violations(ContractMode::Development)
            .iter()
            .map(ContractViolation::code)
            .collect::<Vec<_>>(),
        vec!["ENTITLEMENTS_NOT_EMPTY"]
    );
}

#[test]
fn an_empty_der_container_reads_as_no_entitlements() {
    // The other half of the same measurement: ArkDeck's shipped component
    // carries `7005020101b000` — an empty container, which must read as empty
    // rather than as unparseable.
    let binary = macho(Signed {
        entitlements: Some(Entitlements::Der(vec![
            0x70, 0x05, 0x02, 0x01, 0x01, 0xb0, 0x00,
        ])),
        ..developer_id()
    });
    let code = packaging::read(&binary).expect("a signed fixture reads");
    assert!(
        code.slices[0]
            .signature
            .as_ref()
            .expect("signed")
            .entitlements
            .is_empty()
    );
    assert!(code.violations(ContractMode::Release).is_empty());
}

#[test]
fn every_forbidden_key_carries_the_reason_it_is_forbidden() {
    for (key, why) in packaging::FORBIDDEN_ENTITLEMENTS {
        assert!(!why.is_empty(), "{key} has no explanation");
        assert!(
            why.len() > 30,
            "{key}'s explanation is too short to help anyone: {why:?}"
        );
    }
}

// ------------------------------------------------------------------ the parser

#[test]
fn a_non_macho_file_is_refused_rather_than_guessed_at() {
    assert_eq!(packaging::read(b"#!/bin/sh\n"), Err(ReadError::NotMachO));
    assert_eq!(packaging::read(&[]), Err(ReadError::NotMachO));
}

#[test]
fn a_big_endian_macho_is_refused_rather_than_misread() {
    // Saying nothing about a binary is not the same as saying it is clean.
    let mut bytes = vec![0u8; 64];
    bytes[..4].copy_from_slice(&0xcffa_edfeu32.to_le_bytes());
    assert_eq!(
        packaging::read(&bytes),
        Err(ReadError::UnsupportedByteOrder)
    );
}

#[test]
fn a_load_command_that_would_loop_forever_is_refused() {
    // A zero-length load command is the classic way to hang a Mach-O reader.
    let mut bytes = macho_unsigned();
    // The first load command's size field, right after the 32-byte header.
    bytes[36..40].copy_from_slice(&0u32.to_le_bytes());
    assert_eq!(
        packaging::read(&bytes),
        Err(ReadError::Malformed("load command size"))
    );
}

#[test]
fn mutated_binaries_never_panic_the_reader() {
    // The property is not that mutants parse — it is that no input reaches a
    // panic, a hang, or an unbounded allocation. Seeded so a failure is
    // reproducible from the seed rather than from a lucky Tuesday
    // (`fuzz/README.md`).
    let bases = [
        macho(developer_id()),
        macho_unsigned(),
        fat(&[(0x0100_000c, macho(developer_id()))]),
        macho(Signed {
            entitlements: Some(Entitlements::Der(vec![
                0x70, 0x05, 0x02, 0x01, 0x01, 0xb0, 0x00,
            ])),
            ..developer_id()
        }),
    ];
    for base in &bases {
        for seed in 0..1_500u64 {
            let input = mutate(seed, base);
            // Any outcome but a panic is acceptable; the harness fails by
            // unwinding, so there is nothing to assert on the happy path.
            if let Ok(code) = packaging::read(&input) {
                let _ = code.violations(ContractMode::Release);
                let _ = code.summary();
            }
        }
    }
}

/// xorshift64*, so a seed reproduces a run exactly.
struct Rng(u64);

impl Rng {
    fn next_u64(&mut self) -> u64 {
        let mut x = self.0;
        x ^= x >> 12;
        x ^= x << 25;
        x ^= x >> 27;
        self.0 = x;
        x.wrapping_mul(0x2545_f491_4f6c_dd1d)
    }

    fn below(&mut self, bound: usize) -> usize {
        if bound == 0 {
            0
        } else {
            (self.next_u64() % bound as u64) as usize
        }
    }
}

/// Mutations aimed at offsets, lengths and magics — the fields this reader
/// trusts to find the next structure.
fn mutate(seed: u64, base: &[u8]) -> Vec<u8> {
    let mut rng = Rng(seed | 1);
    let mut out = base.to_vec();
    for _ in 0..1 + rng.below(6) {
        if out.is_empty() {
            break;
        }
        match rng.below(5) {
            0 => {
                let index = rng.below(out.len());
                out[index] = (rng.next_u64() & 0xff) as u8;
            }
            1 => {
                let index = rng.below(out.len());
                out[index] ^= 1 << rng.below(8);
            }
            2 => {
                let cut = rng.below(out.len());
                out.truncate(cut);
            }
            3 => {
                // A four-byte field set to 0xffffffff: the shape that turns an
                // offset into an out-of-bounds read if it is not checked.
                let index = rng.below(out.len().saturating_sub(4).max(1));
                for slot in out.iter_mut().skip(index).take(4) {
                    *slot = 0xff;
                }
            }
            _ => {
                let start = rng.below(out.len());
                let length = rng.below(out.len() - start).min(128);
                for slot in out[start..start + length].iter_mut() {
                    *slot = 0;
                }
            }
        }
    }
    out
}

// ------------------------------------------------------------------- fixtures

enum Entitlements {
    Xml(&'static str),
    Der(Vec<u8>),
}

struct Signed {
    cpu_type: u32,
    identifier: &'static str,
    team: Option<&'static str>,
    ad_hoc: bool,
    linker_signed: bool,
    hardened_runtime: bool,
    entitlements: Option<Entitlements>,
}

/// The shipped `arkforged` signing shape.
fn developer_id() -> Signed {
    Signed {
        cpu_type: 0x0100_000c,
        identifier: "com.arkdeck.agentd.arkforged",
        team: Some("8AQTYW5FKR"),
        ad_hoc: false,
        linker_signed: false,
        hardened_runtime: true,
        entitlements: Some(Entitlements::Xml(
            "<?xml version=\"1.0\" encoding=\"UTF-8\"?><plist version=\"1.0\"><dict/></plist>",
        )),
    }
}

/// A 64-bit Mach-O with one `LC_CODE_SIGNATURE` and an embedded SuperBlob.
fn macho(spec: Signed) -> Vec<u8> {
    let signature = super_blob(&spec);
    let mut out = mach_header(spec.cpu_type, 1);
    // LC_CODE_SIGNATURE: cmd, size, dataoff, datasize. The region follows the
    // header immediately, which is not where a linker would put it and is not
    // something this reader has an opinion about.
    let data_offset = (out.len() + 16) as u32;
    out.extend_from_slice(&0x1du32.to_le_bytes());
    out.extend_from_slice(&16u32.to_le_bytes());
    out.extend_from_slice(&data_offset.to_le_bytes());
    out.extend_from_slice(&(signature.len() as u32).to_le_bytes());
    out.extend_from_slice(&signature);
    out
}

fn macho_unsigned() -> Vec<u8> {
    let mut out = mach_header(0x0100_000c, 1);
    // One LC_SEGMENT_64-shaped command that is not a signature, so the reader
    // has something to walk past.
    out.extend_from_slice(&0x19u32.to_le_bytes());
    out.extend_from_slice(&24u32.to_le_bytes());
    out.extend_from_slice(&[0u8; 16]);
    out
}

fn mach_header(cpu_type: u32, command_count: u32) -> Vec<u8> {
    let mut out = Vec::new();
    out.extend_from_slice(&0xfeed_facfu32.to_le_bytes()); // MH_MAGIC_64
    out.extend_from_slice(&cpu_type.to_le_bytes());
    out.extend_from_slice(&0u32.to_le_bytes()); // cpusubtype
    out.extend_from_slice(&2u32.to_le_bytes()); // MH_EXECUTE
    out.extend_from_slice(&command_count.to_le_bytes());
    out.extend_from_slice(&0u32.to_le_bytes()); // sizeofcmds, unread
    out.extend_from_slice(&0u32.to_le_bytes()); // flags
    out.extend_from_slice(&0u32.to_le_bytes()); // reserved
    out
}

/// A `CSMAGIC_EMBEDDED_SIGNATURE` SuperBlob holding a CodeDirectory and,
/// optionally, one entitlements blob.
fn super_blob(spec: &Signed) -> Vec<u8> {
    let directory = code_directory(spec);
    let entitlements = spec
        .entitlements
        .as_ref()
        .map(|entitlements| match entitlements {
            Entitlements::Xml(xml) => (5u32, blob(0xfade_7171, xml.as_bytes())),
            Entitlements::Der(der) => (7u32, blob(0xfade_7172, der)),
        });

    let count = 1 + entitlements.iter().count() as u32;
    let index_size = 12 + 8 * count as usize;
    let mut body = Vec::new();
    let mut index = Vec::new();

    index.extend_from_slice(&0u32.to_be_bytes()); // CSSLOT_CODEDIRECTORY
    index.extend_from_slice(&((index_size + body.len()) as u32).to_be_bytes());
    body.extend_from_slice(&directory);

    if let Some((slot, encoded)) = &entitlements {
        index.extend_from_slice(&slot.to_be_bytes());
        index.extend_from_slice(&((index_size + body.len()) as u32).to_be_bytes());
        body.extend_from_slice(encoded);
    }

    let mut out = Vec::new();
    out.extend_from_slice(&0xfade_0cc0u32.to_be_bytes());
    out.extend_from_slice(&((index_size + body.len()) as u32).to_be_bytes());
    out.extend_from_slice(&count.to_be_bytes());
    out.extend_from_slice(&index);
    out.extend_from_slice(&body);
    out
}

fn blob(magic: u32, body: &[u8]) -> Vec<u8> {
    let mut out = Vec::new();
    out.extend_from_slice(&magic.to_be_bytes());
    out.extend_from_slice(&((body.len() + 8) as u32).to_be_bytes());
    out.extend_from_slice(body);
    out
}

/// A version 0x20400 CodeDirectory — the version both real binaries measured on
/// 2026-08-16 carry, and the first one where `teamOffset` is present.
fn code_directory(spec: &Signed) -> Vec<u8> {
    let mut flags = 0u32;
    if spec.ad_hoc {
        flags |= 0x0000_0002;
    }
    if spec.linker_signed {
        flags |= 0x0002_0000;
    }
    if spec.hardened_runtime {
        flags |= 0x0001_0000;
    }

    // Fixed fields run to offset 88 for this version; the strings follow.
    const FIXED: usize = 88;
    let identifier_offset = FIXED;
    let team_offset = match spec.team {
        Some(_) => identifier_offset + spec.identifier.len() + 1,
        None => 0,
    };

    let mut out = Vec::new();
    out.extend_from_slice(&0xfade_0c02u32.to_be_bytes()); // magic
    out.extend_from_slice(&0u32.to_be_bytes()); // length, patched below
    out.extend_from_slice(&0x0002_0400u32.to_be_bytes()); // version
    out.extend_from_slice(&flags.to_be_bytes());
    out.extend_from_slice(&0u32.to_be_bytes()); // hashOffset
    out.extend_from_slice(&(identifier_offset as u32).to_be_bytes());
    out.extend_from_slice(&0u32.to_be_bytes()); // nSpecialSlots
    out.extend_from_slice(&0u32.to_be_bytes()); // nCodeSlots
    out.extend_from_slice(&0u32.to_be_bytes()); // codeLimit
    out.push(32); // hashSize
    out.push(2); // hashType: sha256
    out.push(0); // platform
    out.push(12); // pageSize
    out.extend_from_slice(&0u32.to_be_bytes()); // spare2
    out.extend_from_slice(&0u32.to_be_bytes()); // scatterOffset
    out.extend_from_slice(&(team_offset as u32).to_be_bytes());
    out.extend_from_slice(&0u32.to_be_bytes()); // spare3
    out.extend_from_slice(&0u64.to_be_bytes()); // codeLimit64
    out.extend_from_slice(&0u64.to_be_bytes()); // execSegBase
    out.extend_from_slice(&0u64.to_be_bytes()); // execSegLimit
    out.extend_from_slice(&0u64.to_be_bytes()); // execSegFlags
    assert_eq!(
        out.len(),
        FIXED,
        "the fixed CodeDirectory fields are 88 bytes"
    );

    out.extend_from_slice(spec.identifier.as_bytes());
    out.push(0);
    if let Some(team) = spec.team {
        out.extend_from_slice(team.as_bytes());
        out.push(0);
    }

    let length = out.len() as u32;
    out[4..8].copy_from_slice(&length.to_be_bytes());
    out
}

/// A universal binary wrapping already-built thin images.
fn fat(images: &[(u32, Vec<u8>)]) -> Vec<u8> {
    let header = 8 + images.len() * 20;
    let mut out = Vec::new();
    out.extend_from_slice(&0xcafe_babeu32.to_be_bytes());
    out.extend_from_slice(&(images.len() as u32).to_be_bytes());

    let mut offset = header;
    for (cpu_type, image) in images {
        out.extend_from_slice(&cpu_type.to_be_bytes());
        out.extend_from_slice(&0u32.to_be_bytes()); // cpusubtype
        out.extend_from_slice(&(offset as u32).to_be_bytes());
        out.extend_from_slice(&(image.len() as u32).to_be_bytes());
        out.extend_from_slice(&12u32.to_be_bytes()); // align
        offset += image.len();
    }
    for (_, image) in images {
        out.extend_from_slice(image);
    }
    out
}

#[test]
fn the_architecture_names_match_the_ones_codesign_prints() {
    assert_eq!(Architecture(0x0100_000c).to_string(), "arm64");
    assert_eq!(Architecture(0x0100_0007).to_string(), "x86_64");
    assert_eq!(Architecture(0x4242).to_string(), "cputype 0x00004242");
}
