//! The evidence ledger is checked, not just written.
//!
//! `docs/evidence/ledger.md` is the AF-V3 acceptance item "bluetool static
//! evidence 纳入 ledger" and "exact unknown list". A ledger that can drift from
//! the code is a ledger that will, so this suite parses it and asserts it
//! against the things it claims to describe.
//!
//! The property that matters most: **no DAYU600 evidence gate is PASS**, and no
//! code path can produce an executable DAYU600 plan while that is true
//! (architecture.md 17.5, 25.17).

use arkforge_artifact::pac;
use arkforge_transport::transcript::{self, TranscriptProvenance};

const LEDGER: &str = include_str!("../../../docs/evidence/ledger.md");
const DAYU600_PROFILE: &str = include_str!("../../../profiles/dayu600.yaml");
const DAYU600_TRANSCRIPT: &str =
    include_str!("../../../transcripts/dayu600-research-synthetic.yaml");

/// Rows of the gate table: `| # | gate | status | blocker |`.
fn gate_rows() -> Vec<(u32, String, String)> {
    let mut rows = Vec::new();
    let mut in_table = false;
    for line in LEDGER.lines() {
        if line.starts_with("| # | 门 | 状态 | 阻塞 |") {
            in_table = true;
            continue;
        }
        if in_table {
            if !line.starts_with('|') {
                break;
            }
            let cells: Vec<&str> = line.split('|').map(str::trim).collect();
            // ["", number, gate, status, blocker, ""]
            if cells.len() < 5 {
                continue;
            }
            let Ok(number) = cells[1].parse::<u32>() else {
                continue; // the `|---|` separator row
            };
            rows.push((
                number,
                cells[2].to_string(),
                cells[3].replace("**", "").to_string(),
            ));
        }
    }
    rows
}

#[test]
fn the_ledger_lists_all_eighteen_gates() {
    let rows = gate_rows();
    assert_eq!(
        rows.len(),
        18,
        "architecture.md 17.5 defines eighteen gates"
    );
    for (index, (number, _, _)) in rows.iter().enumerate() {
        assert_eq!(*number, index as u32 + 1, "gates are numbered 1..18");
    }
}

#[test]
fn no_dayu600_evidence_gate_is_pass() {
    // The single assertion this whole file exists for. A gate marked PASS
    // without the evidence behind it is how a device gets bricked by a plan
    // nobody could justify.
    for (number, gate, status) in gate_rows() {
        assert_ne!(
            status, "PASS",
            "gate {number} ({gate}) claims PASS; no DAYU600 evidence has been obtained"
        );
        assert!(
            status == "MISSING" || status == "HELD",
            "gate {number} ({gate}) has unrecognized status {status:?}"
        );
    }
}

#[test]
fn gate_eighteen_is_held_rather_than_passed() {
    // "No force/experimental bypass" is a property to keep, not evidence to
    // obtain. Recording it as PASS would suggest it is finished.
    let (_, gate, status) = gate_rows()
        .into_iter()
        .find(|(number, _, _)| *number == 18)
        .expect("gate 18 exists");
    assert_eq!(status, "HELD", "{gate}");
}

#[test]
fn every_unknown_in_the_ledger_is_carried_by_the_parser() {
    // The ledger's unknown table and the parser's list are the same list, so a
    // fact cannot be quietly dropped from one of them.
    let ledger_ids: Vec<String> = LEDGER
        .lines()
        .filter(|line| line.starts_with("| UNI-U"))
        .filter_map(|line| line.split('|').nth(1).map(|cell| cell.trim().to_string()))
        .collect();
    let parser_ids: Vec<&str> = pac::DAYU600_EXECUTION_UNKNOWNS
        .iter()
        .map(|(id, _)| *id)
        .collect();

    assert_eq!(
        ledger_ids.len(),
        parser_ids.len(),
        "ledger lists {ledger_ids:?}, parser lists {parser_ids:?}"
    );
    for id in &parser_ids {
        assert!(
            ledger_ids.iter().any(|ledger| ledger == id),
            "{id} is in the parser but not the ledger"
        );
    }
    for id in &ledger_ids {
        assert!(
            parser_ids.contains(&id.as_str()),
            "{id} is in the ledger but not the parser"
        );
    }
}

#[test]
fn the_ledger_records_ad004_as_static_only() {
    let row = LEDGER
        .lines()
        .find(|line| line.starts_with("| AD-004 |"))
        .expect("AD-004 is the DAYU600 static evidence entry");
    assert!(row.contains("uis7885"), "{row}");
    assert!(row.contains("CmdDloader"), "{row}");
    // The status must say the evidence is static, because that is the whole
    // limit on what it can support.
    assert!(
        row.contains("仅静态") || row.to_lowercase().contains("static"),
        "AD-004 must record that it is static-only: {row}"
    );
    // And the ledger must state the boundary explicitly.
    assert!(LEDGER.contains("### AD-004 的边界"));
}

#[test]
fn the_ledger_keeps_the_no_hardware_pass_rule() {
    assert!(
        LEDGER.contains("simulation / plan-only 不记 real hardware pass"),
        "architecture.md 24.1's rule must be in the ledger"
    );
    assert!(LEDGER.contains("许可未知默认不可再分发"));
    assert!(LEDGER.contains("D / U 不能独立支持 execute"));
}

#[test]
fn the_dayu600_profile_references_only_evidence_the_ledger_confirms() {
    let profile = arkforge_core::profile::load(DAYU600_PROFILE).unwrap();
    let refs: Vec<&str> = profile
        .evidence_refs
        .iter()
        .map(|reference| reference.as_str())
        .collect();
    assert_eq!(
        refs,
        vec!["AD-004"],
        "the only confirmed DAYU600 evidence is AD-004"
    );
    for reference in refs {
        assert!(
            LEDGER.contains(&format!("| {reference} |")),
            "{reference} is referenced but not in the ledger"
        );
    }
}

#[test]
fn no_dayu600_artefact_in_this_repository_claims_to_be_a_capture() {
    // AF-V3 acceptance: plan-only must never be recorded as a hardware pass.
    let parsed = transcript::parse(DAYU600_TRANSCRIPT).unwrap();
    assert_eq!(parsed.provenance, TranscriptProvenance::Synthetic);
    assert!(!parsed.provenance.supports_protocol_claims());

    // And the ledger says the same about it, in the same words.
    assert!(LEDGER.contains("dayu600-research-synthetic.yaml"));
    assert!(LEDGER.contains("**synthetic**"));
}

#[test]
fn the_ledger_does_not_claim_the_pac_module_is_a_parser() {
    // Section 4's table row, not the section 2 pointer at the module.
    let row = LEDGER
        .lines()
        .find(|line| line.starts_with('|') && line.contains("src/pac.rs"))
        .expect("the ledger records the pac module's evidence standing");
    assert!(
        row.contains("非 PAC parser") || row.contains("结构观测器"),
        "{row}"
    );
}

/// AD-017 is an *open* boundary, and it has to stay visible as one.
///
/// A durability claim that quietly upgrades from "ordered against process
/// death" to "durable" is exactly the kind of drift a ledger exists to catch,
/// so both the ledger row and the architecture section that states the limit
/// are asserted here rather than left to a reader's diligence.
#[test]
fn the_durability_boundary_stays_recorded_as_open() {
    let row = LEDGER
        .lines()
        .find(|line| line.starts_with("| AD-017 |"))
        .expect("AD-017 is the durability boundary entry");
    assert!(
        row.contains("open"),
        "AD-017 must stay open until a power-loss experiment exists: {row}"
    );
    assert!(
        row.contains("F_FULLFSYNC"),
        "AD-017 must name what would be needed to close it: {row}"
    );

    const ARCHITECTURE: &str = include_str!("../../../docs/architecture.md");
    assert!(
        ARCHITECTURE.contains("#### 13.2.1 durability 的边界（AD-017）"),
        "the durability limit must appear in the architecture, not only in a module comment"
    );
    assert!(
        ARCHITECTURE.contains("不声明掉电安全"),
        "13.2.1 must say plainly that power-loss safety is not claimed"
    );
}

/// AD-007 is closed, and the thing that closed it has to still be there.
///
/// A resolved row is a claim that some mechanism now holds the property. If the
/// mechanism is deleted and the row is not, the ledger has become a record of
/// what was once true — which is worse than having said nothing, because it
/// reads as current.
#[test]
fn the_entitlement_deadlock_stays_closed_by_something_that_exists() {
    let row = LEDGER
        .lines()
        .find(|line| line.starts_with("| AD-007 |"))
        .expect("AD-007 is the entitlement deadlock entry");
    assert!(row.contains("resolved"), "{row}");
    assert!(
        row.contains("AFD-0003"),
        "AD-007 must name what closed it: {row}"
    );

    // The native daemon is the only release input after NRU-004. The same
    // Mach-O reader remains the fail-closed check at native bind.
    {
        let source = arkforged::packaging::ARKFORGED_ENTITLEMENTS;
        assert!(
            arkforged::packaging::plist_keys(source).is_empty(),
            "an entitlement key appeared in a file AD-007 says is empty"
        );
    }

    // And the check that enforces it, on the shape that caused AD-007 in the
    // first place. Asserted through the ledger test rather than only in the
    // packaging suite because this is the ledger's own claim.
    let sandboxed = "<plist><dict><key>com.apple.security.app-sandbox</key><true/></dict></plist>";
    assert_eq!(
        arkforged::packaging::plist_keys(sandboxed),
        vec!["com.apple.security.app-sandbox"],
        "the reader must still see the key that cannot run here"
    );
}

/// AD-023 is closed by removing the unshippable component from the package,
/// not by weakening its dependency-closure finding.
#[test]
fn the_unshippable_toolchain_finding_is_closed_by_native_packaging() {
    let row = LEDGER
        .lines()
        .find(|line| line.starts_with("| AD-023 |"))
        .expect("AD-023 is the unshippable toolchain entry");
    assert!(
        row.contains("resolved"),
        "AD-023 closes only after the vendor component leaves packaging: {row}"
    );
    assert!(
        row.contains("libusb"),
        "AD-023 must name the dependency that makes it unshippable: {row}"
    );
    assert!(
        LEDGER.contains("### AD-023 对 toolchain 摘要的后果"),
        "the consequence for the maturity combination must be written down, not implied"
    );
    let package_script = include_str!("../../../packaging/macos/package-arkforge.sh");
    assert!(!package_script.contains("rkdeveloptool"));
    assert!(!package_script.contains("RKDEVELOPTOOL"));
}

/// The AF-V2 flash claim is bound to one specific run, not to optimism.
///
/// Until 2026-08-18 this test pinned "device write count 0". The first real
/// flash passed that day (native RockUSB, NRU-004), so the pin inverts: the
/// acceptance document must cite the exact job and its evidence record, must
/// still refuse a blanket "AF-V2 accepted" reading, and must keep naming what
/// remains unverified. A pass claim that names no evidence is the same drift
/// the zero-claim used to guard against, in the other direction.
#[test]
fn the_af_v2_flash_claim_is_bound_to_recorded_evidence() {
    const ACCEPTANCE: &str = include_str!("../../../docs/evidence/AF-V2-acceptance.md");
    const GREEN_RUN: &str = include_str!(
        "../../../docs/evidence/runs/2026-08-19-dayu200-green-flash-and-native-cutover.md"
    );

    // The claim names its job and its evidence file.
    assert!(
        ACCEPTANCE.contains("job-a4b7d539571082b1958ebaaf2c14bd2c"),
        "the acceptance document must cite the job that passed"
    );
    assert!(
        ACCEPTANCE.contains("runs/2026-08-19-dayu200-green-flash-and-native-cutover.md"),
        "the acceptance document must point at the run record"
    );
    // The run record carries the identities that make the claim checkable:
    // the first pass (fixed-tool era, run A) and the post-retirement native
    // pass (run B), each with its job, plan digest, and daemon identity.
    for pinned in [
        // Run A — the AF-V2 acceptance pass, on the since-retired fixed tool.
        "job-a4b7d539571082b1958ebaaf2c14bd2c",
        "JOB-000001A013991062",
        "c8837ff0137b037e06b96129fce71951337b58d051a2bc5c006dfff015eebc5c",
        // Run B — the vendor-free native pass after c049a11.
        "job-b00e006a1fbe9d6de388efab4138b9a2",
        "JOB-000001A0180894B7",
        "dde51435593d77027d5c111d00711c95b69bd2331ea6137fae2c025efe30c4cb",
        "f3dfc624f24c0e7ebd586b12acd0d64c145721f17e96db5557e07b2fbb1766d9",
        // Both runs verified the same flashed build.
        "OpenHarmony-7.0.0.37",
    ] {
        assert!(
            GREEN_RUN.contains(pinned),
            "the run record must pin {pinned}"
        );
    }
    // The record must keep the backend attribution honest in both directions.
    assert!(
        GREEN_RUN.contains("fixed-tool") && GREEN_RUN.contains("NativeRockUsbPort"),
        "the run record must attribute each pass to its actual backend"
    );

    // Still no blanket acceptance, and the open boundaries stay named.
    assert!(
        ACCEPTANCE.contains("**本文仍不整体宣布「AF-V2 已验收」**"),
        "one green run is one acceptance item, not the whole list"
    );
    for open_boundary in ["掉电", "SIGKILL", "multi-device"] {
        assert!(
            ACCEPTANCE.contains(open_boundary),
            "the document must keep naming the unverified boundary {open_boundary}"
        );
    }
    // Maturity promotion stays a separate decision (AD-025).
    assert!(
        ACCEPTANCE.contains("ProductionVerified 的提升不随本次通过自动发生"),
        "a campaign pass must not read as a maturity promotion"
    );
    // The ledger's own rule (§5): plan-only never counts as a hardware pass.
    assert!(
        LEDGER.contains("**simulation / plan-only 不记 real hardware pass**"),
        "the ledger rule this document is written under must still be there"
    );
}
