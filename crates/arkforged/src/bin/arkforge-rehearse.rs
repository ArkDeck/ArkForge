//! `arkforge-rehearse` — run the whole DAYU200 flash plan, minus the writes.
//!
//! architecture.md 15.1 read-only diagnostics. This imports the real archive,
//! materializes the real plan through the real Provider, stages the real
//! images, and then walks every action in order:
//!
//! - read-only actions (`ld`, `ppt`, `rl`) are **executed against the attached
//!   device**, so the mode, the device's own partition table and the medium's
//!   read face are measured rather than assumed;
//! - the nine writes are lowered to the exact argv the executor would spawn,
//!   every precondition is evaluated, and then nothing is dispatched.
//!
//! # Why this stops short of writing
//!
//! Not caution — architecture. AF-V2's goal is "ArkDeck uses ArkForge to
//! complete a real DAYU200 flash", and ArkForge holds no authority
//! (architecture.md 3, 8). A write needs a StepPermit, a permit needs an
//! authority to mint it, and this repository deliberately cannot mint one: the
//! architecture guard forbids `arkforged` from even naming the minting function
//! (8.6). A tool here that flashed a device would have had to invent a second
//! authority, which is the one thing the whole design exists to prevent.
//!
//! So this proves everything up to the permit, and the remaining gap is a
//! decision about who holds the authority, not code that is missing.

use arkforge_artifact::cas::{CasQuota, ContentAddressedStore};
use arkforge_artifact::manifest::ArtifactManifest;
use arkforge_artifact::staging::{stage_members, StagedMember};
use arkforge_artifact::dayu200;
use arkforge_core::identity::{HostPlatform, ToolchainIdentity, ToolchainKind, Version};
use arkforge_core::ids::{OpaqueId, PlanId};
use arkforge_core::plan::PlanMaterialization;
use arkforge_core::profile::{self, DeviceProfile};
use arkforge_core::{AuthorityBindingRef, AuthorityNamespace};
use arkforge_provider::rockchip::{publish_af_v1_maturity, RockchipProvider};
use arkforge_provider::rockchip_execute::{
    execute_action, ExecutionSession, RockUsbCommand, StagedImage, StoredAction,
};
use arkforged::dispatch::HostFixedToolPort;
use arkforge_provider::{
    FlashIntent, FlashProvider, MaterializeRequest, MaturityRegistry, ProbeContext,
};
use arkforge_transport::usb::{IoRegEnumerator, UsbTransport};
use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};
use std::time::Instant;

const DEFAULT_RKDEVELOPTOOL: &str = "/opt/homebrew/bin/rkdeveloptool";

fn main() {
    let arguments: Vec<String> = std::env::args().skip(1).collect();
    match run(&arguments) {
        Ok(()) => {}
        Err(message) => {
            eprintln!("arkforge-rehearse: {message}");
            std::process::exit(1);
        }
    }
}

fn usage() -> String {
    concat!(
        "usage: arkforge-rehearse --archive <file> --profile <file> --store <dir> \\\n",
        "                        --staging <dir> [--rkdeveloptool <path>] [--skip-staging]\n",
        "\n",
        "  --archive         the firmware archive to import\n",
        "  --profile         the DeviceProfile to plan against\n",
        "  --store           content-addressed store root\n",
        "  --staging         job-owned directory for extracted images\n",
        "  --rkdeveloptool   the pinned tool (default: /opt/homebrew/bin/rkdeveloptool)\n",
        "  --skip-staging    lower and check the writes without extracting ~3.9 GB\n",
        "\n",
        "Read-only. The nine writes are lowered and checked, never dispatched.\n"
    )
    .to_string()
}

struct Options {
    archive: PathBuf,
    profile: PathBuf,
    store: PathBuf,
    staging: PathBuf,
    rkdeveloptool: PathBuf,
    skip_staging: bool,
}

fn parse(arguments: &[String]) -> Result<Options, String> {
    let mut archive = None;
    let mut profile = None;
    let mut store = None;
    let mut staging = None;
    let mut rkdeveloptool = PathBuf::from(DEFAULT_RKDEVELOPTOOL);
    let mut skip_staging = false;

    let mut index = 0usize;
    while index < arguments.len() {
        let next = |index: &mut usize| -> Result<String, String> {
            *index += 1;
            arguments.get(*index).cloned().ok_or_else(usage)
        };
        match arguments[index].as_str() {
            "--archive" => archive = Some(PathBuf::from(next(&mut index)?)),
            "--profile" => profile = Some(PathBuf::from(next(&mut index)?)),
            "--store" => store = Some(PathBuf::from(next(&mut index)?)),
            "--staging" => staging = Some(PathBuf::from(next(&mut index)?)),
            "--rkdeveloptool" => rkdeveloptool = PathBuf::from(next(&mut index)?),
            "--skip-staging" => skip_staging = true,
            "--help" | "-h" => {
                print!("{}", usage());
                std::process::exit(0);
            }
            other => return Err(format!("unexpected argument {other:?}\n\n{}", usage())),
        }
        index += 1;
    }

    Ok(Options {
        archive: archive.ok_or_else(usage)?,
        profile: profile.ok_or_else(usage)?,
        store: store.ok_or_else(usage)?,
        staging: staging.ok_or_else(usage)?,
        rkdeveloptool,
        skip_staging,
    })
}

fn run(arguments: &[String]) -> Result<(), String> {
    let options = parse(arguments)?;

    // ---- 1. Profile -------------------------------------------------------
    let source = std::fs::read_to_string(&options.profile)
        .map_err(|error| format!("{}: {error}", options.profile.display()))?;
    let device_profile = profile::load(&source).map_err(|error| error.to_string())?;
    println!("profile        {}", device_profile.id);
    let blockers = device_profile.execution_blockers();
    if blockers.is_empty() {
        println!("               no execution blockers");
    } else {
        for blocker in &blockers {
            println!("  BLOCKER      {} {blocker}", blocker.id());
        }
    }

    // ---- 2. Artifact ------------------------------------------------------
    let store = ContentAddressedStore::open(&options.store, CasQuota::dayu200_default())
        .map_err(|error| error.to_string())?;
    let size = std::fs::metadata(&options.archive)
        .map_err(|error| format!("{}: {error}", options.archive.display()))?
        .len();
    let file = std::fs::File::open(&options.archive).map_err(|error| error.to_string())?;
    let started = Instant::now();
    let imported = store
        .import(file, size, None)
        .map_err(|error| error.to_string())?;
    println!(
        "archive        {} ({size} bytes, imported in {:.2}s{})",
        imported.digest,
        started.elapsed().as_secs_f64(),
        if imported.deduplicated {
            ", deduplicated"
        } else {
            ""
        }
    );

    let started = Instant::now();
    let manifest = dayu200::inspect(
        store
            .open_object(&imported.digest)
            .map_err(|error| error.to_string())?,
    )
    .map_err(|error| error.to_string())?;
    println!(
        "manifest       {} ({}, inspected in {:.2}s)",
        manifest.digest().map_err(|error| error.to_string())?,
        manifest.confidence.as_str(),
        started.elapsed().as_secs_f64()
    );
    for (key, value) in &manifest.build_facts {
        println!("  build fact   {key} = {value}");
    }
    for unknown in &manifest.execution_relevant_unknowns {
        println!("  unknown      {} — {}", unknown.id, unknown.summary);
    }

    // ---- 3. The attached device ------------------------------------------
    let transport = UsbTransport::new(&device_profile, Box::new(IoRegEnumerator));
    let now = now_epoch_ms();
    let observations = transport
        .observe_now(now)
        .map_err(|error| error.to_string())?;
    println!("\nobservations   {}", observations.len());
    for observation in &observations {
        println!(
            "  {} mode={} strength={}",
            observation.observation_id,
            observation.mode.as_str(),
            observation.identity_strength.as_str()
        );
    }
    let observation = observations
        .first()
        .ok_or("no device this profile recognizes is attached")?;

    let provider = RockchipProvider::new();
    let probe = provider
        .probe(&ProbeContext {
            transport: &transport,
            observation,
            profile: &device_profile,
        })
        .map_err(|error| error.to_string())?;
    println!(
        "probe          candidate={} factsDigest={}",
        probe
            .profile_candidate
            .as_ref()
            .map(|candidate| candidate.id.to_string())
            .unwrap_or_else(|| "none".into()),
        probe.facts_digest
    );
    for (key, value) in &probe.protocol_facts {
        println!("  {key} = {value}");
    }

    // ---- 4. The plan ------------------------------------------------------
    let mut maturity = MaturityRegistry::new();
    let toolchain = pinned_toolchain(&options.rkdeveloptool)?;
    publish_af_v1_maturity(
        &mut maturity,
        &provider,
        &device_profile,
        &toolchain,
        &HostPlatform::current(),
        arkforge_core::digest::sha256(b"arkforge-rehearse-driver-facts"),
        arkforge_core::digest::sha256(b"arkforge-rehearse-evidence-set"),
    )
    .map_err(|error| error.to_string())?;

    let artifact_hex = imported.digest.to_string();
    let request = MaterializeRequest {
        plan_id: PlanId::new(format!("PLAN-{}", &artifact_hex[..12].to_uppercase()))
            .map_err(|error| error.to_string())?,
        intent: FlashIntent::FullRestore,
        artifact: &manifest,
        artifact_id: OpaqueId::new(&artifact_hex[..32]).map_err(|error| error.to_string())?,
        profile: &device_profile,
        probe: &probe,
        authority_binding: AuthorityBindingRef {
            // No authority is paired with this tool, and inventing a namespace
            // that looks like one would put a false identity in the record.
            authority_namespace: AuthorityNamespace::new("unbound")
                .map_err(|error| error.to_string())?,
            binding_id: OpaqueId::new("REHEARSAL").map_err(|error| error.to_string())?,
            binding_revision: 0,
            stable_identity_digest: probe.facts_digest,
        },
        toolchain,
        host_platform: HostPlatform::current(),
        driver_facts_digest: arkforge_core::digest::sha256(b"arkforge-rehearse-driver-facts"),
        evidence_set_digest: arkforge_core::digest::sha256(b"arkforge-rehearse-evidence-set"),
        created_at_epoch_ms: now,
        plan_lifetime_ms: 3_600_000,
    };

    let materialized = provider
        .materialize_with_private_plan(&request, &maturity)
        .map_err(|error| error.to_string())?;
    // An assessment still carries its private plan: architecture.md 6.3 requires
    // a gated plan to be auditable, and "here is exactly what I would have run"
    // is the only form that audit can take.
    let Some(private_plan) = materialized.private_plan else {
        return Err("the provider produced no private plan".into());
    };
    match &materialized.materialization {
        PlanMaterialization::Executable(envelope) => println!(
            "\nplan           {} EXECUTABLE ({} public steps, {} private actions)",
            envelope.plan_digest,
            envelope.public_steps.len(),
            private_plan.actions.len()
        ),
        PlanMaterialization::Assessment(assessment) => {
            println!(
                "\nplan           ASSESSMENT ({} private actions): {}",
                private_plan.actions.len(),
                assessment.availability.as_str()
            );
            for unknown in &assessment.unknowns {
                println!("  gated by     {} — {}", unknown.id, unknown.summary);
            }
        }
    }

    // ---- 5. Staging -------------------------------------------------------
    let staged = if options.skip_staging {
        println!("\nstaging        skipped (--skip-staging)");
        BTreeMap::new()
    } else {
        stage(&options, &store, &imported.digest, &manifest, &private_plan)?
    };

    // ---- 6. Walk every action --------------------------------------------
    let port = HostFixedToolPort::open(&options.rkdeveloptool)?;
    println!("\nrkdeveloptool  {}", options.rkdeveloptool.display());
    println!("      sha256   {}", port.digest());

    let scratch = options.staging.join("scratch");
    std::fs::create_dir_all(&scratch).map_err(|error| error.to_string())?;
    let mut session = ExecutionSession::new(
        staged
            .iter()
            .map(|(name, member)| {
                (
                    name.clone(),
                    StagedImage {
                        member: member.member.clone(),
                        path: member.path.clone(),
                        size_bytes: member.size_bytes,
                        sha256: member.sha256,
                    },
                )
            })
            .collect(),
    );

    println!("\nactions");
    let mut withheld = 0usize;
    for action in &private_plan.actions {
        let decoded = match StoredAction::decode(action) {
            Ok(decoded) => decoded,
            Err(error) => {
                println!("  {:<10} UNDECODABLE {error}", action.action_id);
                continue;
            }
        };

        match &decoded {
            // A write is lowered, checked, and withheld.
            StoredAction::WritePartition {
                partition,
                member,
                begin_sector,
            } => {
                withheld += 1;
                let image = staged.get(member);
                let argv = RockUsbCommand::WriteByName {
                    partition: partition.clone(),
                    image: image
                        .map(|image| image.path.clone())
                        .unwrap_or_else(|| PathBuf::from(format!("<unstaged:{member}>"))),
                }
                .argv();
                println!(
                    "  {:<10} WITHHELD    {}",
                    action.action_id,
                    argv.join(" ")
                );
                report_write_preconditions(
                    &mut session,
                    &device_profile,
                    partition,
                    member,
                    *begin_sector,
                    image,
                );
            }
            StoredAction::ManagedControl {
                control_action,
                expect,
            } => {
                println!(
                    "  {:<10} AUTHORITY   {control_action} (ManagedDeviceControlPort, \
                     architecture.md 9.2)",
                    action.action_id
                );
                for (key, value) in expect {
                    println!("               expects      {key} = {value}");
                }
            }
            // `rd` reboots the board. A rehearsal that dispatched it would be
            // changing device state under a tool that says it does not — the
            // first run of this tool did exactly that, and the board only
            // escaped because it was not in Loader mode to hear it.
            StoredAction::ResetDevice => {
                withheld += 1;
                println!(
                    "  {:<10} WITHHELD    {} (reboots the device)",
                    action.action_id,
                    RockUsbCommand::ResetDevice.argv().join(" ")
                );
            }
            _ => match execute_action(
                &decoded,
                action,
                &mut session,
                &device_profile,
                &port,
                &scratch,
            ) {
                Ok(outcome) => {
                    println!(
                        "  {:<10} {:<11} {}",
                        action.action_id,
                        outcome.disposition.as_str(),
                        summarize(&outcome.facts)
                    );
                    if let Some(verification) = &outcome.verification {
                        println!("               verification {}", verification.as_str());
                    }
                }
                Err(error) => {
                    println!("  {:<10} REFUSED     {error}", action.action_id);
                }
            },
        }
    }

    let _ = std::fs::remove_dir_all(&scratch);
    println!(
        "\n{withheld} device-changing action(s) lowered and withheld. Dispatching one needs a \
         StepPermit, and a permit needs an authority this build cannot be (architecture.md 8.6)."
    );
    Ok(())
}

fn report_write_preconditions(
    session: &mut ExecutionSession,
    profile: &DeviceProfile,
    partition: &str,
    member: &str,
    begin_sector: u64,
    image: Option<&StagedMember>,
) {
    let allowed = profile
        .allowed_targets
        .iter()
        .find(|target| target.partition.as_str() == partition);
    match allowed {
        Some(target) if target.offset_sectors == begin_sector => {
            println!("               profile      allows {partition} at sector {begin_sector}")
        }
        Some(target) => println!(
            "               profile      DISAGREES: profile says {}, plan says {begin_sector}",
            target.offset_sectors
        ),
        None => println!("               profile      REFUSES {partition}"),
    }

    match session
        .observed_table()
        .and_then(|table| table.entries.iter().find(|entry| entry.name == partition))
    {
        Some(entry) if entry.offset_sectors == begin_sector => {
            let sectors = image
                .map(|image| image.size_bytes.div_ceil_u64(512))
                .unwrap_or(0);
            match entry.size_sectors {
                Some(span) if sectors > span => println!(
                    "               device       REFUSES: image needs {sectors} sectors, span is {span}"
                ),
                Some(span) => println!(
                    "               device       {partition} @{begin_sector}, {span} sectors to \
                     the next partition, image needs {sectors}"
                ),
                None => println!(
                    "               device       {partition} @{begin_sector} runs to the end of \
                     the medium"
                ),
            }
        }
        Some(entry) => println!(
            "               device       DISAGREES: device says {}, plan says {begin_sector}",
            entry.offset_sectors
        ),
        None => println!("               device       has no partition named {partition}"),
    }

    match image {
        Some(image) => match (StagedImage {
            member: image.member.clone(),
            path: image.path.clone(),
            size_bytes: image.size_bytes,
            sha256: image.sha256,
        })
        .revalidate()
        {
            Ok(()) => println!(
                "               image        {member} revalidated ({} bytes, {})",
                image.size_bytes, image.sha256
            ),
            Err(error) => println!("               image        REFUSED {error}"),
        },
        None => println!("               image        {member} is not staged"),
    }
}

/// `u64::div_ceil` is unstable on this toolchain.
trait DivCeil {
    fn div_ceil_u64(self, denominator: u64) -> u64;
}

impl DivCeil for u64 {
    fn div_ceil_u64(self, denominator: u64) -> u64 {
        self / denominator + u64::from(self % denominator != 0)
    }
}

fn stage(
    options: &Options,
    store: &ContentAddressedStore,
    digest: &arkforge_core::Sha256Digest,
    manifest: &ArtifactManifest,
    private_plan: &arkforge_core::projection::StoredProviderPlan,
) -> Result<BTreeMap<String, StagedMember>, String> {
    let wanted: BTreeSet<String> = private_plan
        .actions
        .iter()
        .filter_map(|action| match StoredAction::decode(action) {
            Ok(StoredAction::WritePartition { member, .. }) => Some(member),
            _ => None,
        })
        .collect();
    println!("\nstaging        {} member(s) into {}", wanted.len(), options.staging.display());
    std::fs::create_dir_all(&options.staging).map_err(|error| error.to_string())?;

    let started = Instant::now();
    let report = stage_members(
        store.open_object(digest).map_err(|error| error.to_string())?,
        manifest,
        &wanted,
        &options.staging,
    )
    .map_err(|error| error.to_string())?;
    println!(
        "               {} bytes in {:.2}s ({:.1} MiB/s)",
        report.bytes_written,
        started.elapsed().as_secs_f64(),
        report.bytes_written as f64 / 1_048_576.0 / started.elapsed().as_secs_f64()
    );
    Ok(report.members)
}

fn summarize(facts: &[(OpaqueId, String)]) -> String {
    facts
        .iter()
        .map(|(key, value)| {
            let value: String = value.chars().take(60).collect();
            format!("{key}={value}")
        })
        .collect::<Vec<_>>()
        .join(" ")
}

fn now_epoch_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|elapsed| elapsed.as_millis() as u64)
        .unwrap_or(0)
}

fn pinned_toolchain(path: &Path) -> Result<ToolchainIdentity, String> {
    let digest = file_digest(path)?;
    Ok(ToolchainIdentity {
        id: OpaqueId::new("rkdeveloptool").map_err(|error| error.to_string())?,
        kind: ToolchainKind::FixedTool,
        version: Version::new(1, 32, 0),
        backend_digest: digest,
        upstream_ref: None,
    })
}

fn file_digest(path: &Path) -> Result<arkforge_core::Sha256Digest, String> {
    use std::io::Read;
    let mut file =
        std::fs::File::open(path).map_err(|error| format!("{}: {error}", path.display()))?;
    let mut hasher = arkforge_core::digest::Sha256::new();
    let mut buffer = vec![0u8; 1 << 16];
    loop {
        let read = file
            .read(&mut buffer)
            .map_err(|error| format!("{}: {error}", path.display()))?;
        if read == 0 {
            break;
        }
        hasher.update(&buffer[..read]);
    }
    Ok(hasher.finalize())
}

/// Compile-time proof that nothing in this file can spawn a write.
///
/// The port takes an argv the Provider lowered; this asserts the one lowering
/// this file performs for a write is only ever printed. If a future edit passed
/// a `WriteByName` command to `port.run`, this test would not catch it — but
/// the `WITHHELD` branch above has no `port` in scope at all, which does.
#[cfg(test)]
mod tests {
    use super::*;
    use arkforge_provider::rockchip_execute::ExecutionError;

    #[test]
    fn the_withheld_write_lowers_to_the_argv_a_real_flash_would_use() {
        let argv = RockUsbCommand::WriteByName {
            partition: "system".into(),
            image: PathBuf::from("/staging/system.img"),
        }
        .argv();
        assert_eq!(argv, vec!["wlx", "system", "/staging/system.img"]);
    }

    #[test]
    fn an_execution_error_names_the_authority_boundary_rather_than_a_missing_feature() {
        let error = ExecutionError::RequiresAuthority {
            control_action: "enter-updater".into(),
        };
        assert!(error.to_string().contains("authority's control port"));
    }
}
