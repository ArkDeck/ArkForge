//! Contract a consumer runs against a pinned ArkForge revision.
//!
//! ```text
//! cargo test -p arkforge-arkdeck-adapter --test consumer_contract
//! ```

use arkforge_arkdeck_adapter::control::CONTROL_TABLE_VERSION;
use arkforge_arkdeck_adapter::{
    ARKDECK_CONTRACT_VERSION, BOOT_LOOP_RESTORATION_SCHEMA, CONTRACT_TAG,
    FLASH_VERIFICATION_SCHEMA, FLASH_VERIFY_EXIT_FAILED, FLASH_VERIFY_EXIT_MATCHED,
    FLASH_VERIFY_EXIT_PARTIAL, MAPPING_TABLE_VERSION, PROTOCOL_VERSION, SKIP_MOUNTED_WRITABLE,
};
use arkforge_ipc::contract;

#[test]
fn the_adapter_pin_matches_the_ipc_contract() {
    assert_eq!(PROTOCOL_VERSION, contract::PROTOCOL_VERSION);
    assert_eq!(PROTOCOL_VERSION, "1.0");
    assert_eq!(CONTRACT_TAG, contract::CONTRACT_TAG);
    assert_eq!(CONTRACT_TAG, "arkforge-contract-v1.0.0");
    assert_eq!(ARKDECK_CONTRACT_VERSION, "arkforge.arkdeck-contract/1.0.0");
    assert!(ARKDECK_CONTRACT_VERSION.ends_with("1.0.0"));
    assert!(CONTRACT_TAG.ends_with("1.0.0"));
    assert_eq!(MAPPING_TABLE_VERSION, "arkforge.arkdeck-step-map/v1");
    assert_eq!(CONTROL_TABLE_VERSION, "arkforge.arkdeck-control-map/v1");
    assert_eq!(arkforge_ipc::PROTOCOL_MAJOR, 1);
    assert_eq!(arkforge_ipc::PROTOCOL_MINOR, 0);
}

#[test]
fn flash_verify_and_boot_loop_constants_match_the_ipc_contract() {
    assert_eq!(FLASH_VERIFY_EXIT_MATCHED, 0);
    assert_eq!(FLASH_VERIFY_EXIT_FAILED, 7);
    assert_eq!(FLASH_VERIFY_EXIT_PARTIAL, 9);
    assert_eq!(
        FLASH_VERIFY_EXIT_MATCHED,
        contract::FLASH_VERIFY_EXIT_MATCHED
    );
    assert_eq!(FLASH_VERIFY_EXIT_FAILED, contract::FLASH_VERIFY_EXIT_FAILED);
    assert_eq!(
        FLASH_VERIFY_EXIT_PARTIAL,
        contract::FLASH_VERIFY_EXIT_PARTIAL
    );
    assert_eq!(
        FLASH_VERIFICATION_SCHEMA,
        contract::FLASH_VERIFICATION_SCHEMA
    );
    assert_eq!(
        BOOT_LOOP_RESTORATION_SCHEMA,
        contract::BOOT_LOOP_RESTORATION_SCHEMA
    );
    assert_eq!(SKIP_MOUNTED_WRITABLE, "mounted-writable-after-boot");
    assert_eq!(contract::flash_verify_exit_code(0, 0), 0);
    assert_eq!(contract::flash_verify_exit_code(0, 1), 9);
    assert_eq!(contract::flash_verify_exit_code(2, 1), 7);
}

#[test]
fn the_api_versions_file_matches_the_adapter_tables() {
    let versions = include_str!("../../../api/versions.yaml");
    for needle in [
        &format!("contractTag: {CONTRACT_TAG}"),
        &format!("protocolVersion: \"{PROTOCOL_VERSION}\""),
        &format!("arkdeckContract: {ARKDECK_CONTRACT_VERSION}"),
        &format!("mappingTable: {MAPPING_TABLE_VERSION}"),
        &format!("controlTable: {CONTROL_TABLE_VERSION}"),
        "proto: api/arkforge.proto",
    ] {
        assert!(
            versions.contains(needle),
            "api/versions.yaml lacks {needle}"
        );
    }
}

#[test]
fn the_compatibility_doc_names_this_pin_and_the_verification_exits() {
    let doc = include_str!("../../../docs/arkdeck-contract.md");
    for needle in [
        CONTRACT_TAG,
        ARKDECK_CONTRACT_VERSION,
        MAPPING_TABLE_VERSION,
        CONTROL_TABLE_VERSION,
        PROTOCOL_VERSION,
        FLASH_VERIFICATION_SCHEMA,
        BOOT_LOOP_RESTORATION_SCHEMA,
        SKIP_MOUNTED_WRITABLE,
        "cargo test -p arkforge-ipc --test consumer_contract",
        "cargo test -p arkforge-arkdeck-adapter --test consumer_contract",
        "exit 0",
        "exit 7",
        "exit 9",
        "check-arkforge-pin.py",
        "{ git, rev }",
        "40-hex",
    ] {
        assert!(
            doc.contains(needle),
            "docs/arkdeck-contract.md lacks {needle}"
        );
    }
    assert!(
        !doc.contains("git tag -a") && !doc.contains("git push origin arkforge-contract"),
        "docs/arkdeck-contract.md still tells a consumer to create a contract tag"
    );
    assert!(
        !doc.contains("tag ="),
        "docs/arkdeck-contract.md still shows a Cargo tag pin"
    );
}
