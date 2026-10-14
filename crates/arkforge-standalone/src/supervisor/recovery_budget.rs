//! Append-only grants for independent recovery epochs, never old-intent retries.
use super::*;
use std::collections::{BTreeMap, BTreeSet};

pub(super) const MAX_EPOCHS: u64 = 16;
const DOMAIN: &[u8] = b"arkforge/standalone/recovery-budget/v1\0";

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct Seal {
    pub ordinal: u64,
    pub previous: Vec<u8>,
}

impl Seal {
    pub fn encode(&self) -> Vec<u8> {
        let mut bytes = Vec::new();
        wire::write_uint64(&mut bytes, 1, self.ordinal);
        wire::write_bytes(&mut bytes, 2, &self.previous);
        bytes
    }

    pub fn decode(bytes: &[u8]) -> Result<Self, StandaloneError> {
        let mut seal = Self {
            ordinal: 0,
            previous: Vec::new(),
        };
        let mut reader = wire::Reader::new(bytes);
        while let Some((field, value)) = reader.next_field().map_err(blocked)? {
            match field {
                1 => seal.ordinal = value.as_u64().map_err(blocked)?,
                2 => seal.previous = value.as_bytes().map_err(blocked)?.to_vec(),
                _ => return Err(blocked("Unknown recovery budget policy field.")),
            }
        }
        if seal.encode() != bytes
            || !(1..=MAX_EPOCHS).contains(&seal.ordinal)
            || seal.previous.len() != 32
        {
            return Err(blocked("Invalid recovery budget grant seal."));
        }
        Ok(seal)
    }
}

pub(super) fn path(runtime: &Path, ordinal: u64, extension: &str) -> PathBuf {
    runtime
        .join("authority")
        .join("recovery-grants")
        .join(format!("{ordinal:02}.{extension}"))
}

fn entries(root: &Path) -> Result<Vec<PathBuf>, StandaloneError> {
    if !root.try_exists().map_err(blocked)? {
        return Ok(Vec::new());
    }
    require_plain(root, true)?;
    let mut paths = std::fs::read_dir(root)
        .map_err(blocked)?
        .map(|entry| entry.map(|entry| entry.path()).map_err(blocked))
        .collect::<Result<Vec<_>, _>>()?;
    if paths.len() > 4096 {
        return Err(blocked("Recovery ledger exceeds its storage bound."));
    }
    paths.sort();
    Ok(paths)
}

/// Preserve the exact legacy reservation/proof/correlation bytes. A partially
/// published legacy source set counts once, but never becomes a usable grant.
fn legacy(runtime: &Path) -> Result<(u64, Vec<u8>), StandaloneError> {
    let mut bytes = DOMAIN.to_vec();
    let mut epochs = BTreeSet::new();
    let root = runtime.join("authority").join("recovery");
    let paths = entries(&root)?;
    for file in &paths {
        if file.extension().and_then(|v| v.to_str()) == Some("job") {
            let reserved = file.with_extension("reserved");
            if !reserved.try_exists().map_err(blocked)? {
                return Err(blocked(
                    "A legacy correlation lost its consumed reservation.",
                ));
            }
        }
        if file.extension().and_then(|v| v.to_str()) != Some("reserved") {
            continue;
        }
        let body = read(file)?;
        let proof = Proof::decode(&body)?;
        if proof.budget.is_some()
            || !proof
                .sources()
                .any(|source| super::path(runtime, &source.assessment.job_id, "reserved") == *file)
        {
            return Err(blocked(
                "Legacy reservation has an invalid source correlation.",
            ));
        }
        let sealed = read(&super::path(
            runtime,
            &proof.assessment.candidate_plan_id,
            "proof",
        ))?;
        if sealed != body {
            return Err(blocked("Legacy proof differs from its spent reservation."));
        }
        epochs.insert(sha256(&body).to_hex());
        wire::write_string(
            &mut bytes,
            1,
            file.file_name()
                .and_then(|v| v.to_str())
                .ok_or_else(|| blocked("Invalid legacy record name."))?,
        );
        wire::write_bytes(&mut bytes, 2, &body);
        let correlation = file.with_extension("job");
        if correlation.try_exists().map_err(blocked)? {
            let job = read(&correlation)?;
            JobId::new(std::str::from_utf8(&job).map_err(blocked)?).map_err(blocked)?;
            wire::write_bytes(&mut bytes, 3, &job);
        }
    }
    Ok((epochs.len() as u64, sha256(&bytes).as_bytes().to_vec()))
}

struct History {
    count: u64,
    digest: Vec<u8>,
    grants: Vec<Proof>,
}

fn history(runtime: &Path) -> Result<History, StandaloneError> {
    let (mut count, mut digest) = legacy(runtime)?;
    if count > MAX_EPOCHS {
        return Err(blocked("Historical recovery budget is exhausted."));
    }
    let mut records = BTreeMap::new();
    let mut adjuncts = Vec::new();
    for file in entries(&runtime.join("authority").join("recovery-grants"))? {
        let extension = file
            .extension()
            .and_then(|v| v.to_str())
            .ok_or_else(|| blocked("Invalid recovery grant record name."))?;
        if extension == "pending" {
            continue;
        }
        let ordinal = file
            .file_stem()
            .and_then(|v| v.to_str())
            .and_then(|v| v.parse::<u64>().ok())
            .filter(|v| (1..=MAX_EPOCHS).contains(v))
            .ok_or_else(|| blocked("Invalid recovery grant ordinal."))?;
        if file != path(runtime, ordinal, extension) {
            return Err(blocked("Noncanonical recovery grant record name."));
        }
        match extension {
            "grant" => {
                records.insert(ordinal, read(&file)?);
            }
            "job" | "complete" => {
                require_plain(&file, false)?;
                adjuncts.push(ordinal);
            }
            _ => return Err(blocked("Unknown recovery grant record.")),
        }
    }
    if adjuncts
        .iter()
        .any(|ordinal| !records.contains_key(ordinal))
    {
        return Err(blocked(
            "A recovery correlation or completion lost its grant.",
        ));
    }
    let mut grants = Vec::new();
    for (ordinal, body) in records {
        let proof = Proof::decode(&body)?;
        let expected = Seal {
            ordinal: count + 1,
            previous: digest.clone(),
        };
        if proof.budget.as_ref() != Some(&expected) || ordinal != expected.ordinal {
            return Err(blocked(
                "Recovery consumption history is missing, changed or reordered.",
            ));
        }
        if read(&super::path(
            runtime,
            &proof.assessment.candidate_plan_id,
            "proof",
        ))? != body
        {
            return Err(blocked(
                "A committed grant differs from its exact sealed plan proof.",
            ));
        }
        let mut chain = DOMAIN.to_vec();
        wire::write_bytes(&mut chain, 1, &digest);
        wire::write_bytes(&mut chain, 2, &body);
        digest = sha256(&chain).as_bytes().to_vec();
        count += 1;
        grants.push(proof);
    }
    Ok(History {
        count,
        digest,
        grants,
    })
}

pub(super) fn next(runtime: &Path) -> Result<Seal, StandaloneError> {
    let history = history(runtime)?;
    if history.count >= MAX_EPOCHS {
        return Err(blocked(
            "Recovery budget exhausted: all 16 independent epochs are consumed; failures, expiry and restart never refund a grant.",
        ));
    }
    Ok(Seal {
        ordinal: history.count + 1,
        previous: history.digest,
    })
}

pub(super) fn reserve(runtime: &Path, proof: &Proof) -> Result<(), StandaloneError> {
    let seal = proof
        .budget
        .as_ref()
        .ok_or_else(|| blocked("Missing independent recovery grant."))?;
    if *seal != next(runtime)? {
        return Err(blocked(
            "Recovery budget changed after planning; this plan cannot consume a grant.",
        ));
    }
    // Competing plans target the same immutable ordinal. Only one hard-link
    // publication succeeds. A crash after this point consumes the entire grant.
    persist(&path(runtime, seal.ordinal, "grant"), &proof.encode())
}

pub(super) fn grants(runtime: &Path) -> Result<Vec<Proof>, StandaloneError> {
    Ok(history(runtime)?.grants)
}

pub(super) fn validate(runtime: &Path, proof: &Proof) -> Result<u64, StandaloneError> {
    let seal = proof
        .budget
        .as_ref()
        .ok_or_else(|| blocked("Missing recovery budget seal."))?;
    if !history(runtime)?.grants.iter().any(|grant| grant == proof) {
        return Err(blocked(
            "No committed independent grant matches this exact proof.",
        ));
    }
    Ok(seal.ordinal)
}
