//! Parity against ArkDeck's pinned DAYU200 archive facts, on the real archive.
//!
//! The fixture tests in `dayu200_inspect.rs` prove the parser's *rules*. This
//! one proves the parser reaches the same answer ArkDeck reached, on the same
//! 730 MB vendor daily build, byte for byte — the 17 member digests, the 15
//! partition entries, and the build facts ArkDeck pins as
//! `runtimeProductModel` / `runtimeBuildVersion`.
//!
//! Two independent implementations agreeing on a real artifact is the only
//! thing that makes ArkForge's manifest usable as ArkDeck's input. A fixture
//! cannot establish it: I wrote the fixture, so it agrees with me by
//! construction.
//!
//! The archive is 730 MB and is not in the repository. Set
//! `ARKFORGE_DAYU200_ARCHIVE` to its path to run this; without it the test
//! reports that it did not run rather than claiming a pass it did not earn.
//! Evidence AD-016 records the run that did execute it.

use arkforge_artifact::cas::{CasQuota, ContentAddressedStore};
use arkforge_artifact::dayu200;
use arkforge_artifact::manifest::{GrammarBranch, MemberRole};

/// ArkDeck `RockchipFlashProfile.dayu200`: archive identity.
const PINNED_ARCHIVE_SHA256: &str =
    "6a023c738ac585b8a6f537c99f2ab2df95a5359fd6d4dd33150fad62e71f064e";
const PINNED_ARCHIVE_BYTES: u64 = 730_769_584;

/// ArkDeck `RockchipFlashProfile.dayu200.members`, transcribed. Roles are
/// ArkForge's own container-format classification, not ArkDeck's
/// profile-driven one: ArkDeck calls `chip_prod.img` an orphan because its
/// board forbids that partition, which is a device policy ArkForge's parser is
/// forbidden to hold (architecture.md 10.4).
const PINNED_MEMBERS: [(&str, u64, &str, MemberRole); 17] = [
    (
        "boot_linux.img",
        67_108_864,
        "1202a1ba694aaa3d53f104e6374a9aaffd0dba048c3122cf9f4704c4063bd757",
        MemberRole::ImageCandidate,
    ),
    (
        "chip_ckm.img",
        33_554_432,
        "f99c14c2520f618c721c963307ddc72ec47aefb5a71c7b29b268b1b33edcc0db",
        MemberRole::ImageCandidate,
    ),
    (
        "chip_prod.img",
        52_428_800,
        "44797e1616481c6211526358c11056862e04a3595dd81f59e41aec03a384ad29",
        MemberRole::ImageCandidate,
    ),
    (
        "config.cfg",
        10_399,
        "4d06d303faff1d3e530a9d2c9bb22073427b0b498bb4bb438b5177897d86f33c",
        MemberRole::Metadata,
    ),
    (
        "daily_build.log",
        24_507_809,
        "8454628003ab59a4edf28c073b39ec3891cad925283244c3bed0b754ecf35503",
        MemberRole::Metadata,
    ),
    (
        "manifest_tag.xml",
        115_118,
        "71f9293a21d21fb1da67d27b0482b198c62ce042bb80326d62e1a0f35ee12691",
        MemberRole::Metadata,
    ),
    (
        "MiniLoaderAll.bin",
        455_104,
        "1cdd418032195210f191445ed96e2da5ea83d2cfe880c912ebec635839d76542",
        MemberRole::Loader,
    ),
    (
        "parameter.txt",
        788,
        "35464e3f0b883a8a043dd45ae7ab2342c86b7aa27f24aa1e5a0ccfb6f442d048",
        MemberRole::PartitionTable,
    ),
    (
        "ramdisk.img",
        2_366_141,
        "c7e94434b4624ef70a5b9472d4848212a79c89b7a8cb5a453262e56a72e5dec9",
        MemberRole::ImageCandidate,
    ),
    (
        "resource.img",
        5_652_480,
        "208ceef6be9ba6d5781033bf00718b15f54d0210ae2f0e8134d4a5e40a9c13e7",
        MemberRole::ImageCandidate,
    ),
    (
        "sys_prod.img",
        52_428_800,
        "631845214a4ca4da44094165e30509eb2254a601350b56f90197bf78c3aa85d7",
        MemberRole::ImageCandidate,
    ),
    (
        "system.img",
        2_147_483_648,
        "86357e57a183278e1662d55c2d560a35e8e685613bd270f62df42bdf783f0650",
        MemberRole::ImageCandidate,
    ),
    (
        "uboot.img",
        4_194_304,
        "c1c801e45cbb92ee63e14df3dda5d819792e02295525bd53dbf750efb645916d",
        MemberRole::ImageCandidate,
    ),
    (
        "updater_binary",
        3_248_972,
        "250b6ebc32f33088a328804cc918766aa6ea30f1c0acc8e2d08cf3ec7cf8f23f",
        MemberRole::Unclassified,
    ),
    (
        "updater.img",
        20_688_145,
        "907076f10bc295a3712a911c31c7c8f83bb164cdff4d8d9c1c62d3e91c0f637a",
        MemberRole::ImageCandidate,
    ),
    (
        "userdata.img",
        1_468_006_400,
        "ea60e842586208b660b72ae4b507a1f4cabb397e912156f342f30f21907e1255",
        MemberRole::ImageCandidate,
    ),
    (
        "vendor.img",
        268_431_360,
        "b3ffda2b6dbae220361721ee6b78d25e2055ab506e5480b17eacf477ea482360",
        MemberRole::ImageCandidate,
    ),
];

/// The archive's own `parameter.txt`, as ArkForge decodes it. Sectors, in the
/// unit the source encodes; `None` is the grow marker.
const PINNED_PARTITIONS: [(&str, u64, Option<u64>, GrammarBranch); 15] = [
    ("uboot", 8192, Some(8192), GrammarBranch::Fixed),
    ("misc", 16_384, Some(8192), GrammarBranch::Fixed),
    ("bootctrl", 24_576, Some(4096), GrammarBranch::Fixed),
    ("resource", 28_672, Some(12_288), GrammarBranch::Fixed),
    (
        "boot_linux",
        40_960,
        Some(196_608),
        GrammarBranch::FixedBootable,
    ),
    ("ramdisk", 237_568, Some(8192), GrammarBranch::Fixed),
    ("system", 245_760, Some(4_194_304), GrammarBranch::Fixed),
    ("vendor", 4_440_064, Some(2_097_152), GrammarBranch::Fixed),
    ("sys-prod", 6_537_216, Some(102_400), GrammarBranch::Fixed),
    ("chip-prod", 6_639_616, Some(102_400), GrammarBranch::Fixed),
    ("updater", 6_742_016, Some(65_536), GrammarBranch::Fixed),
    ("eng_system", 6_807_552, Some(32_768), GrammarBranch::Fixed),
    ("eng_chipset", 6_840_320, Some(32_768), GrammarBranch::Fixed),
    ("chip_ckm", 6_938_624, Some(131_072), GrammarBranch::Fixed),
    ("userdata", 19_955_712, None, GrammarBranch::RemainderGrow),
];

/// ArkDeck pins these as `runtimeProductModel` / `runtimeBuildVersion`, and its
/// source comment records that the device flashed on 2026-08-04 answered with
/// the second one — while the archive's *file name* says 7.0.0.35.
const PINNED_BUILD_FACTS: [(&str, &str); 3] = [
    ("const.ohos.fullname", "OpenHarmony-7.0.0.36"),
    ("const.product.model", "ohos"),
    ("const.product.name", "OpenHarmony 3.2"),
];

#[test]
fn the_parser_reproduces_arkdecks_pinned_facts_on_the_real_archive() {
    let Some(path) = std::env::var_os("ARKFORGE_DAYU200_ARCHIVE") else {
        eprintln!(
            "skipped: set ARKFORGE_DAYU200_ARCHIVE to the {PINNED_ARCHIVE_BYTES}-byte \
             DAYU200 daily archive to run this parity check"
        );
        return;
    };

    let root = std::env::temp_dir().join(format!("arkforge-parity-{}", std::process::id()));
    let store = ContentAddressedStore::open(&root, CasQuota::dayu200_default()).unwrap();

    let size = std::fs::metadata(&path).unwrap().len();
    assert_eq!(
        size, PINNED_ARCHIVE_BYTES,
        "ARKFORGE_DAYU200_ARCHIVE is not the pinned archive"
    );
    let imported = store
        .import(std::fs::File::open(&path).unwrap(), size, None)
        .unwrap();
    assert_eq!(
        imported.digest.to_string(),
        PINNED_ARCHIVE_SHA256,
        "ARKFORGE_DAYU200_ARCHIVE is not the pinned archive"
    );

    let manifest = dayu200::inspect(store.open_object(&imported.digest).unwrap()).unwrap();
    let _ = std::fs::remove_dir_all(&root);

    assert_eq!(manifest.members.len(), PINNED_MEMBERS.len());
    for (name, size_bytes, sha256, role) in PINNED_MEMBERS {
        let member = manifest
            .member(name)
            .unwrap_or_else(|| panic!("archive is missing pinned member {name}"));
        assert_eq!(member.size_bytes, size_bytes, "{name} size");
        assert_eq!(member.sha256.to_string(), sha256, "{name} digest");
        assert_eq!(member.role, role, "{name} role");
    }

    let table = manifest
        .partition_table
        .as_ref()
        .expect("the archive declares a partition table");
    assert_eq!(table.device, "rk29xxnand");
    assert_eq!(table.entries.len(), PINNED_PARTITIONS.len());
    for (index, (name, offset, size, branch)) in PINNED_PARTITIONS.iter().enumerate() {
        let entry = &table.entries[index];
        assert_eq!(&entry.name, name, "partition {index} name");
        assert_eq!(entry.offset_sectors, *offset, "{name} offset");
        assert_eq!(entry.size_sectors, *size, "{name} size");
        assert_eq!(entry.grammar_branch, *branch, "{name} grammar branch");
    }

    for (key, value) in PINNED_BUILD_FACTS {
        let found = manifest
            .build_facts
            .iter()
            .find(|(id, _)| id.as_str() == key)
            .map(|(_, value)| value.as_str());
        assert_eq!(found, Some(value), "build fact {key}");
    }

    // Everything left open is a member the container format cannot classify.
    // The profile discharges it; the parser does not pretend to.
    let open: Vec<&str> = manifest
        .execution_relevant_unknowns
        .iter()
        .map(|unknown| unknown.id.as_str())
        .collect();
    assert_eq!(open, vec!["RK-A02"]);
    assert_eq!(manifest.unclassified_members, vec!["updater_binary"]);
}
