
fn digest_from(bytes: &[u8]) -> Option<Sha256Digest> {
    if bytes.len() != 32 {
        return None;
    }
    let mut array = [0u8; 32];
    array.copy_from_slice(bytes);
    Some(Sha256Digest::from_bytes(array))
}

fn encode_hex(bytes: &[u8]) -> String {
    const HEX: &[u8; 16] = b"0123456789abcdef";
    let mut out = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        out.push(HEX[(byte >> 4) as usize] as char);
        out.push(HEX[(byte & 0x0f) as usize] as char);
    }
    out
}

fn decode_hex(text: &str) -> Option<Vec<u8>> {
    if !text.len().is_multiple_of(2) || !text.is_ascii() {
        return None;
    }
    fn nibble(byte: u8) -> Option<u8> {
        match byte {
            b'0'..=b'9' => Some(byte - b'0'),
            b'a'..=b'f' => Some(byte - b'a' + 10),
            _ => None,
        }
    }
    let (pairs, remainder) = text.as_bytes().as_chunks::<2>();
    debug_assert!(remainder.is_empty());
    pairs
        .iter()
        .map(|pair| Some(nibble(pair[0])? << 4 | nibble(pair[1])?))
        .collect()
}

fn id(value: &str) -> Result<OpaqueId, JobError> {
    OpaqueId::new(value).map_err(|_| JobError::UnusableIdentifier(value.to_string()))
}

fn recover_receipt_events(records: &[JournalRecord], job_id: &str) -> Vec<JobEvent> {
    let mut events = BTreeMap::<u64, JobEvent>::new();
    for record in records {
        if record.kind != JournalRecordKind::SemanticReceiptRecorded {
            continue;
        }
        let fact = |name: &str| {
            record
                .facts
                .iter()
                .find(|(key, _)| key.as_str() == name)
                .map(|(_, value)| value.as_str())
        };
        let Some(body) = fact("receiptBodyHex").and_then(decode_hex) else {
            continue;
        };
        let Ok(receipt) = ActionReceiptSummary::from_canonical_bytes(&body) else {
            continue;
        };
        if receipt.job_id != job_id
            || fact(fact::JOB_ID) != Some(job_id)
            || fact(fact::PLAN_ID) != Some(receipt.plan_id.as_str())
            || fact(fact::STEP_ID) != Some(receipt.step_id.as_str())
            || fact(fact::ATTEMPT_ID) != Some(receipt.attempt_id.as_str())
            || fact(fact::PERMIT_ID) != Some(receipt.permit_id.as_str())
        {
            continue;
        }
        let Ok(receipt_digest) = receipt.receipt_digest() else {
            continue;
        };
        if fact(fact::RECEIPT_DIGEST) != Some(receipt_digest.to_hex().as_str()) {
            continue;
        }
        let Some(sequence) = fact("receiptEventSequence")
            .and_then(|value| value.parse::<u64>().ok())
            .filter(|sequence| *sequence > 0)
        else {
            continue;
        };
        events.entry(sequence).or_insert_with(|| JobEvent {
            job_id: job_id.to_string(),
            sequence,
            kind: JobEventKind::ActionReceipt,
            at_epoch_ms: record.at_epoch_ms,
            journal_record_sha256: record.record_digest.as_bytes().to_vec(),
            job_state: JobState::Checkpointed.as_str().to_string(),
            receipt: Some(receipt),
            ..JobEvent::default()
        });
    }
    events.into_values().collect()
}
