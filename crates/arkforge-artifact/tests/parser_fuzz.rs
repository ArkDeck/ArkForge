//! Deterministic fuzz harness for the artifact parsers.
//!
//! The property under test is not "the parser accepts good input" — the unit
//! tests cover that. It is that **no input reaches a panic, a hang, or an
//! unbounded allocation**: every rejection is a typed error. A parser that
//! panics on a malformed archive is a denial-of-service surface on the daemon
//! that holds device authority (architecture.md 20.1).
//!
//! Seeded rather than random: a failure here has to be reproducible from the
//! seed printed in the assertion, and CI must not go red on a Tuesday because
//! a lucky byte string appeared. See `fuzz/README.md` for the cargo-fuzz
//! target this mirrors.

use arkforge_artifact::dayu200;
use arkforge_artifact::fixture;
use arkforge_artifact::inflate::GzipReader;
use arkforge_artifact::pac;
use arkforge_artifact::tar::TarReader;
use std::io::Read;

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

/// Mutations chosen to hit framing, lengths and checksums — the fields a
/// parser is most likely to trust.
fn mutate(seed: u64, base: &[u8]) -> Vec<u8> {
    let mut rng = Rng(seed | 1);
    let mut out = base.to_vec();
    let rounds = 1 + rng.below(6);
    for _ in 0..rounds {
        if out.is_empty() {
            break;
        }
        match rng.below(6) {
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
                let index = rng.below(out.len());
                let byte = (rng.next_u64() & 0xff) as u8;
                out.insert(index, byte);
            }
            4 => {
                let count = rng.below(64);
                out.extend(std::iter::repeat(0xffu8).take(count));
            }
            _ => {
                let start = rng.below(out.len());
                let length = rng.below(out.len() - start).min(256);
                for slot in out[start..start + length].iter_mut() {
                    *slot = 0;
                }
            }
        }
    }
    out
}

fn drain<R: Read>(mut reader: R) {
    let mut sink = [0u8; 8192];
    let mut budget = 0u64;
    loop {
        match reader.read(&mut sink) {
            Ok(0) => return,
            Ok(count) => {
                budget += count as u64;
                // A stream that decodes more than a gigabyte from a kilobyte
                // fixture would be a decompression bomb, not a fixture.
                assert!(budget < 1 << 30, "output ran away");
            }
            Err(_) => return,
        }
    }
}

#[test]
fn mutated_archives_never_panic_the_dayu200_parser() {
    let base = fixture::dayu200_archive();
    for seed in 0..4_000u64 {
        let input = mutate(seed, &base);
        // Any outcome is acceptable except a panic; the harness fails by
        // unwinding, so there is nothing to assert on the happy path.
        let _ = dayu200::inspect(input.as_slice());
    }
}

#[test]
fn mutated_gzip_streams_never_panic_the_inflater() {
    let base = fixture::gzip_stored(&fixture::fixture_body("fuzz", 20_000));
    for seed in 0..6_000u64 {
        let input = mutate(seed, &base);
        if let Ok(reader) = GzipReader::new(input.as_slice()) {
            drain(reader);
        }
    }
}

#[test]
fn mutated_tar_streams_never_panic_the_reader() {
    let base = fixture::TarArchiveBuilder::new()
        .add_file("parameter.txt", fixture::PINNED_CMDLINE.as_bytes())
        .add_file("system.img", &fixture::fixture_body("system.img", 4096))
        .finish();
    for seed in 0..6_000u64 {
        let input = mutate(seed, &base);
        let mut reader = TarReader::new(input.as_slice());
        loop {
            match reader.next_member() {
                Ok(Some(header)) => {
                    if reader.read_member_body(&header, |_| {}).is_err() {
                        break;
                    }
                }
                Ok(None) | Err(_) => break,
            }
        }
    }
}

#[test]
fn arbitrary_text_never_panics_the_parameter_grammar() {
    let corpus = [
        fixture::PINNED_CMDLINE,
        "CMDLINE:mtdparts=",
        "CMDLINE:mtdparts=d:",
        "CMDLINE:mtdparts=d:-@0x0(a:grow)",
        "CMDLINE:",
        "",
    ];
    for (index, base) in corpus.iter().enumerate() {
        for seed in 0..2_000u64 {
            let mutated = mutate(seed ^ (index as u64) << 32, base.as_bytes());
            if let Ok(text) = std::str::from_utf8(&mutated) {
                let _ = dayu200::parse_parameter(text, 512);
            }
        }
    }
}

/// A stored-block gzip whose declared lengths lie must not be believed.
#[test]
fn a_lying_stored_block_length_is_rejected() {
    let payload = fixture::fixture_body("stored", 1000);
    let mut stream = fixture::gzip_stored(&payload);
    // Byte 10 is the block header; 11..13 the length, 13..15 its complement.
    stream[11] = 0xff;
    stream[12] = 0xff;
    let reader = GzipReader::new(stream.as_slice()).unwrap();
    let mut out = Vec::new();
    let mut reader = reader;
    assert!(
        reader.read_to_end(&mut out).is_err(),
        "a corrupted stored-block length must not decode"
    );
}

/// The PAC research parser reads containers this project has no specification
/// for, from sources it does not control. A panic there is a denial of service
/// on the daemon that holds device authority, so the property is the same as
/// for the DAYU200 parser: any input, typed outcome, no panic.
#[test]
fn arbitrary_containers_never_panic_the_pac_research_parser() {
    let bases: Vec<Vec<u8>> = vec![
        // Something shaped like a firmware package.
        {
            let mut bytes = b"BP_R1.0.0".to_vec();
            bytes.extend_from_slice(&[0u8; 7]);
            for index in 0..12u8 {
                let start = bytes.len();
                bytes.push(0x02);
                bytes.push(index);
                for character in format!("IMG_{index}").chars() {
                    bytes.push(character as u8);
                    bytes.push(0);
                }
                while bytes.len() - start < 32 {
                    bytes.push(0);
                }
            }
            bytes.extend(fixture::fixture_body("pac-payload", 8_000));
            bytes.extend_from_slice(&[0xffu8; 1024]);
            bytes
        },
        // Something that is not one at all.
        fixture::fixture_body("noise", 12_000),
        vec![0u8; 8_000],
        vec![0xffu8; 8_000],
    ];
    for (index, base) in bases.iter().enumerate() {
        for seed in 0..900u64 {
            let input = mutate(seed ^ ((index as u64) << 40), base);
            let _ = pac::inspect(input.as_slice());
        }
    }
}

/// Whatever a mutated container looks like, the parser cannot be talked into
/// claiming it understands it.
#[test]
fn no_mutated_container_upgrades_the_parser_confidence() {
    use arkforge_artifact::manifest::ParserConfidence;
    let base = {
        let mut bytes = b"BP_R1.0.0".to_vec();
        bytes.extend(fixture::fixture_body("pac", 4_000));
        bytes
    };
    for seed in 0..1_500u64 {
        let input = mutate(seed, &base);
        if let Ok((manifest, _)) = pac::inspect(input.as_slice()) {
            assert_eq!(
                manifest.confidence,
                ParserConfidence::ResearchOnly,
                "seed {seed} produced a non-research manifest"
            );
            assert_eq!(
                manifest.execution_relevant_unknowns.len(),
                pac::DAYU600_EXECUTION_UNKNOWNS.len(),
                "seed {seed} dropped an unknown"
            );
        }
    }
}
