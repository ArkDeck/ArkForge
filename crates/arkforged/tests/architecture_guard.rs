//! Architecture guard.
//!
//! architecture.md 4.3 asks for a dependency-graph and type-boundary guard, and
//! explicitly says a substring scan must not be the *only* guard. So the primary
//! checks here read the workspace's actual dependency edges and the crates'
//! public surfaces; the lexical scan runs afterwards as a secondary net for the
//! one thing a graph cannot see — a device or vendor name written into a
//! neutral crate's own source.
//!
//! AF-V1 acceptance: "Core 不依赖 ArkDeck/vendor".

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};

fn repo_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .unwrap()
        .parent()
        .unwrap()
        .to_path_buf()
}

/// Every workspace crate and the workspace crates it depends on, read from the
/// manifests rather than from a convention.
fn dependency_graph() -> BTreeMap<String, BTreeSet<String>> {
    let root = repo_root();
    let mut graph = BTreeMap::new();
    for directory in ["crates", "adapters"] {
        let base = root.join(directory);
        for entry in std::fs::read_dir(&base).expect("workspace directory") {
            let entry = entry.expect("directory entry");
            let manifest = entry.path().join("Cargo.toml");
            if !manifest.exists() {
                continue;
            }
            let source = std::fs::read_to_string(&manifest).expect("manifest");
            let name = source
                .lines()
                .find_map(|line| line.strip_prefix("name = "))
                .map(|value| value.trim().trim_matches('"').to_string())
                .expect("manifest declares a name");
            let mut dependencies = BTreeSet::new();
            // Only `[dependencies]` shapes the architecture. A dev-dependency
            // exists to let a test observe two crates together and does not put
            // one crate inside the other's shipped graph.
            let mut in_runtime_dependencies = false;
            for line in source.lines() {
                let line = line.trim();
                if line.starts_with('[') {
                    in_runtime_dependencies = line == "[dependencies]";
                    continue;
                }
                if !in_runtime_dependencies {
                    continue;
                }
                if let Some((left, _)) = line.split_once(" = { path =")
                    && left.starts_with("arkforge")
                {
                    dependencies.insert(left.to_string());
                }
            }
            graph.insert(name, dependencies);
        }
    }
    graph
}

#[test]
fn core_depends_on_nothing() {
    let graph = dependency_graph();
    let core = graph
        .get("arkforge-core")
        .expect("arkforge-core is a workspace member");
    assert!(
        core.is_empty(),
        "arkforge-core must be a leaf; it depends on {core:?}"
    );
}

#[test]
fn the_dependency_direction_matches_the_architecture() {
    // architecture.md 4.3:
    //   core ← {authority-api, artifact, transport, provider} ← engine ← ipc/daemon
    //   client → ipc
    //   standalone → client + authority-api + daemon host helpers
    //   presentation frontends → client/standalone
    let graph = dependency_graph();
    let allowed: BTreeMap<&str, BTreeSet<&str>> = BTreeMap::from([
        ("arkforge-core", BTreeSet::new()),
        // Win32 named-pipe/ACL/CSPRNG FFI is confined to this leaf.
        ("arkforge-platform", BTreeSet::new()),
        // CHG-2026-063: the sole IOKit/unsafe containment crate is a leaf.
        ("arkforge-usb", BTreeSet::new()),
        ("arkforge-authority-api", BTreeSet::from(["arkforge-core"])),
        ("arkforge-artifact", BTreeSet::from(["arkforge-core"])),
        ("arkforge-transport", BTreeSet::from(["arkforge-core"])),
        (
            "arkforge-provider",
            BTreeSet::from(["arkforge-core", "arkforge-artifact", "arkforge-transport"]),
        ),
        (
            "arkforge-engine",
            BTreeSet::from([
                "arkforge-core",
                "arkforge-authority-api",
                "arkforge-artifact",
                "arkforge-transport",
                "arkforge-provider",
            ]),
        ),
        ("arkforge-ipc", BTreeSet::from(["arkforge-core"])),
        (
            "arkforge-client",
            BTreeSet::from(["arkforge-ipc", "arkforge-platform"]),
        ),
        (
            "arkforged",
            BTreeSet::from([
                "arkforge-core",
                "arkforge-usb",
                "arkforge-authority-api",
                "arkforge-artifact",
                "arkforge-transport",
                "arkforge-provider",
                "arkforge-engine",
                "arkforge-ipc",
                "arkforge-platform",
            ]),
        ),
        (
            "arkforge-arkdeck-adapter",
            BTreeSet::from(["arkforge-core", "arkforge-authority-api", "arkforge-ipc"]),
        ),
        (
            "arkforge-standalone",
            BTreeSet::from([
                "arkforge-client",
                "arkforge-artifact",
                "arkforge-core",
                "arkforge-authority-api",
                "arkforge-ipc",
                "arkforge-platform",
                "arkforge-transport",
                "arkforged",
            ]),
        ),
        (
            "arkforge-cli",
            BTreeSet::from([
                "arkforge-client",
                "arkforge-standalone",
                "arkforge-core",
                "arkforge-authority-api",
                "arkforge-artifact",
                "arkforge-ipc",
                "arkforge-transport",
                "arkforged",
            ]),
        ),
    ]);

    for (crate_name, dependencies) in &graph {
        let permitted = allowed
            .get(crate_name.as_str())
            .unwrap_or_else(|| panic!("{crate_name} is not in the architecture's crate list"));
        for dependency in dependencies {
            assert!(
                permitted.contains(dependency.as_str()),
                "{crate_name} depends on {dependency}, which architecture.md 4.3 does not permit"
            );
        }
    }
}

#[test]
fn unsafe_is_confined_to_the_usb_ffi_crate() {
    // AFD-0001 as revised by CHG-2026-063.  The direct IOKit ABI requires raw
    // pointers, but no protocol, daemon, provider, or neutral core crate may
    // acquire an unsafe surface as a side effect of that decision.
    let root = repo_root();
    for directory in ["crates", "adapters"] {
        for entry in std::fs::read_dir(root.join(directory)).expect("workspace directory") {
            let entry = entry.expect("directory entry");
            if matches!(
                entry.file_name().to_str(),
                Some("arkforge-usb" | "arkforge-platform")
            ) {
                continue;
            }
            let src = entry.path().join("src");
            let mut sources = Vec::new();
            collect(&src, &mut sources);
            for (path, source) in sources {
                let code = code_only(&source);
                assert!(
                    !code.contains("unsafe "),
                    "{} contains unsafe code outside crates/arkforge-usb",
                    path.display()
                );
            }
        }
    }
}

#[test]
fn only_the_standalone_authority_may_compose_the_daemon_library() {
    // The standalone product layer composes the mechanics-side host helpers;
    // CLI and future UI frontends consume that layer instead of each growing a
    // second authority. No neutral or authority-adapter crate may build on the
    // daemon or one authority's adapter.
    let graph = dependency_graph();
    for (crate_name, dependencies) in &graph {
        for forbidden in ["arkforged", "arkforge-arkdeck-adapter"] {
            if matches!(crate_name.as_str(), "arkforge-cli" | "arkforge-standalone")
                && forbidden == "arkforged"
            {
                continue;
            }
            assert!(
                !dependencies.contains(forbidden),
                "{crate_name} must not depend on {forbidden}"
            );
        }
    }
}

fn sources_of(crate_relative_path: &str) -> Vec<(PathBuf, String)> {
    let mut out = Vec::new();
    collect(&repo_root().join(crate_relative_path).join("src"), &mut out);
    out
}

fn collect(directory: &Path, out: &mut Vec<(PathBuf, String)>) {
    let Ok(entries) = std::fs::read_dir(directory) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_dir() {
            collect(&path, out);
        } else if path.extension().map(|ext| ext == "rs").unwrap_or(false)
            && let Ok(source) = std::fs::read_to_string(&path)
        {
            out.push((path, source));
        }
    }
}

/// Strips comments and doc comments, so a *discussion* of Rockchip in a neutral
/// crate is not confused with a *dependency* on it.
fn code_only(source: &str) -> String {
    source
        .lines()
        .map(|line| {
            let trimmed = line.trim_start();
            if trimmed.starts_with("//") {
                ""
            } else {
                line.split("//").next().unwrap_or("")
            }
        })
        .collect::<Vec<_>>()
        .join("\n")
}

#[test]
fn the_neutral_crates_name_no_device_vendor_or_authority_in_code() {
    // Secondary net. The dependency graph above is the primary guard; this
    // catches a name typed directly into a neutral crate's code.
    let forbidden = [
        "dayu200",
        "dayu600",
        "DAYU200",
        "DAYU600",
        "rockchip",
        "Rockchip",
        "unisoc",
        "Unisoc",
        "rkdeveloptool",
        "CmdDloader",
        "RockUSB",
        "rockusb",
        "arkdeck",
        "ArkDeck",
        "uis7885",
    ];
    // `arkforge-artifact` and `arkforge-provider` are the crates architecture.md
    // 4.2 says may hold device modules, so they are excluded by design.
    for crate_path in [
        "crates/arkforge-core",
        "crates/arkforge-authority-api",
        "crates/arkforge-ipc",
        "crates/arkforge-engine",
    ] {
        for (path, source) in sources_of(crate_path) {
            let code = code_only(&source);
            for needle in forbidden {
                assert!(
                    !code.contains(needle),
                    "{} names {needle:?} in code; {crate_path} must stay neutral \
                     (architecture.md 4.3)",
                    path.display()
                );
            }
        }
    }
}

#[test]
fn the_transport_crate_names_no_vendor_protocol_in_code() {
    // Transport may know about USB and modes, but not about a vendor's
    // protocol: those live in the provider (architecture.md 4.2).
    let forbidden = ["rkdeveloptool", "CmdDloader", "wlx", "FDL", "PAC"];
    for (path, source) in sources_of("crates/arkforge-transport") {
        let code = code_only(&source);
        for needle in forbidden {
            assert!(
                !code.contains(needle),
                "{} names {needle:?} in code",
                path.display()
            );
        }
    }
}

#[test]
fn the_daemon_never_mints_a_permit() {
    // architecture.md 8.6: arkforged verifies, it does not mint. The minting
    // function lives behind `authority_side`, and this asserts the daemon never
    // reaches for it.
    for (path, source) in sources_of("crates/arkforged") {
        let code = code_only(&source);
        for needle in ["authority_side", "mint_integrity_tag"] {
            assert!(
                !code.contains(needle),
                "{} references {needle:?}; the daemon may only verify permits",
                path.display()
            );
        }
    }
}

#[test]
fn the_daemon_can_observe_a_real_device() {
    // AD-027. The daemon held only `TranscriptTransport` until 2026-08-17:
    // `UsbTransport::with_ioreg` existed in diagnostic binaries, but nothing
    // wired it into `arkforged`. So
    // `discoverDevices` answered "no devices observed" on a host whose `ioreg`
    // was listing the board, and `materializePlan` — which matches an
    // observation before it probes — could never reach real hardware.
    //
    // A source-level guard rather than a discovery assertion, because the
    // behavioural version would depend on what is plugged into the machine
    // running the tests, and would pass on a clean runner for the same reason
    // the bug survived: nothing was attached to contradict it.
    let source = std::fs::read_to_string(repo_root().join("crates/arkforged/src/service.rs"))
        .expect("the service source");
    let code = code_only(&source);
    assert!(
        code.contains("UsbTransport"),
        "arkforged builds no USB transport; it would see transcripts only, and a daemon that \
         cannot observe the device in front of it cannot materialize a plan for it"
    );
}

#[test]
fn the_workspace_has_no_third_party_runtime_dependencies() {
    // AFD-0001. A dependency added without a decision record would show up as a
    // non-path dependency here.
    let root = repo_root();
    for directory in ["crates", "adapters"] {
        for entry in std::fs::read_dir(root.join(directory)).expect("workspace directory") {
            let entry = entry.expect("directory entry");
            let manifest_path = entry.path().join("Cargo.toml");
            if !manifest_path.exists() {
                continue;
            }
            let manifest = std::fs::read_to_string(&manifest_path).expect("manifest");
            let mut in_dependencies = false;
            for line in manifest.lines() {
                let trimmed = line.trim();
                if trimmed.starts_with('[') {
                    in_dependencies = trimmed.starts_with("[dependencies")
                        || trimmed.starts_with("[dev-dependencies")
                        || trimmed.starts_with("[build-dependencies");
                    continue;
                }
                if !in_dependencies || trimmed.is_empty() || trimmed.starts_with('#') {
                    continue;
                }
                assert!(
                    trimmed.contains("path ="),
                    "{}: {trimmed:?} is not a path dependency; see AFD-0001",
                    manifest_path.display()
                );
            }
        }
    }
    // And no lockfile entries from a registry.
    let lock = root.join("Cargo.lock");
    if lock.exists() {
        let source = std::fs::read_to_string(&lock).expect("lockfile");
        assert!(
            !source.contains("registry+"),
            "Cargo.lock references a registry source; see AFD-0001"
        );
    }
}

#[test]
fn the_public_plan_surface_carries_no_vendor_vocabulary() {
    // A type-boundary check rather than a scan of the provider: whatever the
    // provider writes into a private action, the *public* step encoding must
    // not contain it (architecture.md 6.1, 25.3).
    use arkforge_core::digest::CanonicalCbor;
    use arkforge_core::step::FlashStepKind;

    for kind in FlashStepKind::ALL {
        let rendered = kind.as_str();
        for needle in [
            "rkdeveloptool",
            "wlx",
            "rl",
            "sector",
            "lba",
            "usb",
            "vid",
            "pid",
        ] {
            assert!(
                !rendered.to_lowercase().contains(needle),
                "the public step vocabulary leaks {needle:?} through {rendered:?}"
            );
        }
    }

    // And the kind's canonical encoding is just its name.
    let encoded = FlashStepKind::WriteTarget.to_canonical_bytes().unwrap();
    assert_eq!(
        String::from_utf8_lossy(&encoded[1..]),
        "writeTarget",
        "a step kind encodes as its neutral name and nothing else"
    );
}

#[test]
fn the_materialized_dayu200_plan_would_be_admissible_by_the_arkdeck_registry() {
    // The end-to-end architecture check: a plan this repository materializes
    // maps, step for step, onto published ArkDeck WorkflowStep kinds and meets
    // every registry floor (architecture.md 5.4).
    use arkforge_arkdeck_adapter::check_plan;

    let plan = crate::support::materialize_dayu200_plan();
    let mapped = check_plan(&plan.public_steps)
        .unwrap_or_else(|refusal| panic!("the DAYU200 plan is not admissible: {refusal}"));
    assert_eq!(mapped.len(), 23);
    assert_eq!(mapped[0], "enterUpdater");
    assert_eq!(mapped[1], "probeDevice");
    assert_eq!(mapped[2], "verifyRemoteState");
    assert!(mapped[3..12].iter().all(|kind| *kind == "flashPartition"));
    assert!(
        mapped[12..21]
            .iter()
            .all(|kind| *kind == "verifyRemoteState")
    );
    assert_eq!(mapped[21], "rebootDevice");
    assert_eq!(mapped[22], "verifyRemoteState");
}

mod support {
    use arkforge_artifact::{dayu200, fixture};
    use arkforge_core::digest::sha256;
    use arkforge_core::identity::{
        HostPlatform, MaturityKey, MaturityState, ToolchainIdentity, ToolchainKind, Version,
    };
    use arkforge_core::ids::{OpaqueId, PlanId};
    use arkforge_core::plan::{ExecutionPurpose, FlashPlanEnvelope};
    use arkforge_core::profile;
    use arkforge_core::{
        AuthorityBindingRef, AuthorityNamespace, AuthoritySupportBinding, AuthoritySupportState,
    };
    use arkforge_provider::rockchip::RockchipProvider;
    use arkforge_provider::{
        FlashIntent, FlashProvider, MaterializeRequest, MaturityRegistry, ProbeContext,
    };
    use arkforge_transport::replay::TranscriptTransport;
    use arkforge_transport::{DeviceTransport, TypedDiscoveryFilter, transcript};

    const PROFILE_SOURCE: &str = include_str!("../../../profiles/dayu200.yaml");
    const CAMPAIGN: &str = include_str!("../../../transcripts/dayu200-gj4-ecamp-96effff15.yaml");

    /// Materializes the DAYU200 plan through the executable branch, so the
    /// admission check has a full step list to examine.
    pub fn materialize_dayu200_plan() -> FlashPlanEnvelope {
        let archive = fixture::dayu200_archive();
        let manifest = dayu200::inspect(archive.as_slice()).unwrap();
        let profile = profile::load(PROFILE_SOURCE).unwrap();
        let transport = TranscriptTransport::new(transcript::parse(CAMPAIGN).unwrap());
        let observations = transport
            .discover(&TypedDiscoveryFilter::default(), 0)
            .unwrap();
        let provider = RockchipProvider::new();
        let probe = provider
            .probe(&ProbeContext {
                transport: &transport,
                observation: &observations[0],
                profile: &profile,
            })
            .unwrap();

        let toolchain = ToolchainIdentity {
            id: OpaqueId::new("arkforged-native-rockusb").unwrap(),
            kind: ToolchainKind::NativeProtocol,
            version: Version::new(0, 1, 0),
            backend_digest: sha256(b"native arkforged build"),
            upstream_ref: None,
        };
        let host_platform = HostPlatform::new("macos", "aarch64").unwrap();
        let driver_facts_digest = sha256(b"driver");
        let evidence_set_digest = sha256(b"evidence");

        let mut registry = MaturityRegistry::new();
        registry.publish(
            &MaturityKey {
                provider: provider.identity().clone(),
                profile: profile.identity().unwrap(),
                artifact_format: provider.descriptor().artifact_formats[0].clone(),
                toolchain: toolchain.clone(),
                host_platform: host_platform.clone(),
                driver_facts_digest,
                evidence_set_digest,
            },
            // A test double: publishing this for real needs a DAYU200 pass.
            MaturityState::ProductionVerified,
        );

        let request = MaterializeRequest {
            plan_id: PlanId::new("PLAN-GUARD").unwrap(),
            execution_purpose: ExecutionPurpose::PrimaryFlash,
            intent: FlashIntent::FullRestore,
            artifact: &manifest,
            artifact_id: OpaqueId::new("ART-GUARD").unwrap(),
            profile: &profile,
            probe: &probe,
            authority_binding: AuthorityBindingRef {
                authority_namespace: AuthorityNamespace::new("arkdeck").unwrap(),
                binding_id: OpaqueId::new("TGT-958780b2ffb7").unwrap(),
                binding_revision: 2,
                stable_identity_digest: sha256(b"device"),
            },
            authority_support: AuthoritySupportBinding {
                key_digest: sha256(b"test authority support"),
                state: AuthoritySupportState::ProductionVerified,
            },
            toolchain,
            host_platform,
            driver_facts_digest,
            evidence_set_digest,
            created_at_epoch_ms: 1_754_380_800_000,
            plan_lifetime_ms: 3_600_000,
        };

        match provider.materialize(&request, &registry).unwrap() {
            arkforge_core::plan::PlanMaterialization::Executable(plan) => *plan,
            other => panic!("expected an executable plan, got {other:?}"),
        }
    }
}
