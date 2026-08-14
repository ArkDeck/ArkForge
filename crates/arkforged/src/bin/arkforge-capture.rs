//! `arkforge-capture` — read-only real-device capture.
//!
//! architecture.md 15.1 allows a CLI for read-only and offline diagnostics.
//! This is that tool, and it is the only thing in the repository that can
//! produce a transcript with `provenance: captured` — the one provenance that
//! may back a claim about how hardware behaves (architecture.md 11.4, 24.1).
//!
//! Two boundaries it keeps, both of which the production daemon keeps harder:
//!
//! - **HDC is not driven by argv from anywhere but here.** ArkForge proper
//!   receives a typed [`ManagedDeviceControlAction`] and never an executable
//!   path, a connect key override, or a shell (architecture.md 9.2). This tool
//!   implements that port against the host's hdc *as a diagnostic*; in
//!   production the ArkDeck adapter implements it against ArkDeck's own typed
//!   HDC provider, and the server stays ArkDeck's (9.1).
//! - **No write reaches a partition.** The tool has no subcommand that writes,
//!   and the Rockchip commands it can issue are `ld`, `ppt` and `rl` — list,
//!   print table, read. `wl`/`wlx` appear nowhere in this file.
//!
//! `enter-loader` and `reboot-normal` do change device state (a mode
//! transition), so both require `--i-am-changing-device-mode` on the command
//! line. That flag is the operator saying it out loud.

use arkforge_authority_api::{ManagedControlReceipt, ManagedDeviceControlAction};
use arkforge_core::digest::{sha256, Sha256Digest};
use arkforge_core::ids::OpaqueId;
use arkforge_core::profile::{self, DeviceProfile};
use arkforge_transport::usb::{UsbDeviceRecord, UsbTransport};
use arkforge_transport::{DeviceObservation, SerialEvidence};
use std::path::PathBuf;
use std::process::Command;

fn main() {
    let arguments: Vec<String> = std::env::args().skip(1).collect();
    match run(&arguments) {
        Ok(()) => {}
        Err(message) => {
            eprintln!("arkforge-capture: {message}");
            std::process::exit(1);
        }
    }
}

fn usage() -> String {
    concat!(
        "usage: arkforge-capture <command> --profile <file> [options]\n",
        "\n",
        "read-only commands:\n",
        "  observe                       list every USB device, and those the profile names\n",
        "  transcript --out <file>       write a provenance:captured transcript\n",
        "  facts --target <key>          read product/build facts through the control port\n",
        "  partition-table               print the device's own table (Loader mode, rkdeveloptool ppt)\n",
        "  read-domain                   measure the rl read window (Loader mode)\n",
        "\n",
        "mode-changing commands (require --i-am-changing-device-mode):\n",
        "  enter-loader --target <key>   hdc target boot loader\n",
        "  reboot-normal                 rkdeveloptool rd\n",
        "\n",
        "options:\n",
        "  --hdc <path>                  hdc executable (default: DevEco SDK path)\n",
        "  --rkdeveloptool <path>        rkdeveloptool executable\n",
        "\n",
        "No subcommand writes to a partition.\n"
    )
    .to_string()
}

const DEFAULT_HDC: &str =
    "/Applications/DevEco-Studio.app/Contents/sdk/default/openharmony/toolchains/hdc";
const DEFAULT_RKDEVELOPTOOL: &str = "/opt/homebrew/bin/rkdeveloptool";

struct Options {
    command: String,
    profile_path: Option<PathBuf>,
    out: Option<PathBuf>,
    target: Option<String>,
    hdc: PathBuf,
    rkdeveloptool: PathBuf,
    mode_change_acknowledged: bool,
}

fn parse_options(arguments: &[String]) -> Result<Options, String> {
    let mut options = Options {
        command: String::new(),
        profile_path: None,
        out: None,
        target: None,
        hdc: PathBuf::from(DEFAULT_HDC),
        rkdeveloptool: PathBuf::from(DEFAULT_RKDEVELOPTOOL),
        mode_change_acknowledged: false,
    };
    let mut index = 0usize;
    while index < arguments.len() {
        match arguments[index].as_str() {
            "--profile" => {
                index += 1;
                options.profile_path =
                    Some(PathBuf::from(arguments.get(index).ok_or_else(usage)?));
            }
            "--out" => {
                index += 1;
                options.out = Some(PathBuf::from(arguments.get(index).ok_or_else(usage)?));
            }
            "--target" => {
                index += 1;
                options.target = Some(arguments.get(index).ok_or_else(usage)?.clone());
            }
            "--hdc" => {
                index += 1;
                options.hdc = PathBuf::from(arguments.get(index).ok_or_else(usage)?);
            }
            "--rkdeveloptool" => {
                index += 1;
                options.rkdeveloptool = PathBuf::from(arguments.get(index).ok_or_else(usage)?);
            }
            "--i-am-changing-device-mode" => options.mode_change_acknowledged = true,
            "--help" | "-h" => {
                print!("{}", usage());
                std::process::exit(0);
            }
            other if options.command.is_empty() => options.command = other.to_string(),
            other => return Err(format!("unexpected argument {other:?}\n\n{}", usage())),
        }
        index += 1;
    }
    if options.command.is_empty() {
        return Err(usage());
    }
    Ok(options)
}

fn run(arguments: &[String]) -> Result<(), String> {
    let options = parse_options(arguments)?;
    let profile = load_profile(&options)?;

    match options.command.as_str() {
        "observe" => observe(&profile),
        "transcript" => write_transcript(&options, &profile),
        "facts" => read_facts(&options),
        "partition-table" => partition_table(&options),
        "read-domain" => read_domain(&options),
        "enter-loader" => enter_loader(&options),
        "reboot-normal" => reboot_normal(&options),
        other => Err(format!("unknown command {other:?}\n\n{}", usage())),
    }
}

fn load_profile(options: &Options) -> Result<DeviceProfile, String> {
    let path = options
        .profile_path
        .as_ref()
        .ok_or_else(|| format!("--profile is required\n\n{}", usage()))?;
    let source =
        std::fs::read_to_string(path).map_err(|error| format!("{}: {error}", path.display()))?;
    profile::load(&source).map_err(|error| format!("{}: {error}", path.display()))
}

/// Identifies an executable by its content, so a receipt names the tool that
/// actually ran (architecture.md 12.3: the toolchain digest is part of the
/// maturity combination).
fn tool_digest(path: &PathBuf) -> Result<Sha256Digest, String> {
    let bytes = std::fs::read(path).map_err(|error| format!("{}: {error}", path.display()))?;
    Ok(sha256(&bytes))
}

fn observe(profile: &DeviceProfile) -> Result<(), String> {
    let transport = UsbTransport::with_ioreg(profile);
    let all = transport
        .enumerate_all()
        .map_err(|error| error.to_string())?;
    println!("host sees {} USB device(s)", all.len());
    for record in &all {
        println!(
            "  {:#06x}:{:#06x} loc={:#010x} {:<24} {}",
            record.vendor_id,
            record.product_id,
            record.location_id,
            record.vendor_name.as_deref().unwrap_or("-"),
            record.product_name.as_deref().unwrap_or("-")
        );
    }

    let observations = transport
        .observe_now(now_epoch_ms())
        .map_err(|error| error.to_string())?;
    println!(
        "\nprofile {} recognizes {} of them",
        profile.id,
        observations.len()
    );
    for observation in &observations {
        print_observation(observation);
    }
    if observations.is_empty() {
        println!("  (none — the profile has measured no identity that matches)");
    }
    Ok(())
}

fn print_observation(observation: &DeviceObservation) {
    println!("  {} mode={}", observation.observation_id, observation.mode);
    println!("    identity   {}", observation.identity_strength.as_str());
    println!("    topology   {}", observation.topology_digest);
    println!("    descriptor {}", observation.descriptor_digest);
    match &observation.serial_evidence {
        SerialEvidence::Absent => println!("    serial     (absent)"),
        SerialEvidence::Descriptor { digest } => println!("    serial     {digest} (descriptor)"),
        SerialEvidence::ProtocolUnique { digest } => {
            println!("    serial     {digest} (protocol)")
        }
    }
    for fact in &observation.protocol_identity {
        println!("    {:<10} {}", fact.key, fact.value);
    }
}

fn write_transcript(options: &Options, profile: &DeviceProfile) -> Result<(), String> {
    let out = options
        .out
        .as_ref()
        .ok_or("transcript requires --out <file>")?;
    let transport = UsbTransport::with_ioreg(profile);
    let all = transport
        .enumerate_all()
        .map_err(|error| error.to_string())?;
    let observations = transport
        .observe_now(now_epoch_ms())
        .map_err(|error| error.to_string())?;
    if observations.is_empty() {
        return Err("no device this profile recognizes is attached; refusing to write an empty capture".into());
    }

    let mut document = String::new();
    document.push_str(&format!(
        "# Captured transcript — {} \n\
         #\n\
         # Provenance: captured. Every digest below is derived from bytes the\n\
         # operating system read from the device, not from a string.\n\
         #\n\
         # Captured by `arkforge-capture` on a host where the device was present\n\
         # and observable. What it records is a USB-level observation: identity,\n\
         # topology and descriptor facts. It records no protocol exchange,\n\
         # because this tool speaks no protocol.\n\
         #\n\
         # Serial numbers appear only as digests (architecture.md 11.4).\n\
         \n\
         schemaVersion: arkforge.transcript/v1\n\
         \n\
         transcript:\n\
        \x20 id: {}\n\
        \x20 provenance: captured\n\
        \x20 source: \"arkforge-capture usb observation; {} host USB device(s) present, {} recognized by the profile\"\n\
        \x20 profileId: {}\n\
         \n\
         records:\n",
        profile.id,
        capture_id(&observations),
        all.len(),
        observations.len(),
        profile.id,
    ));

    for (index, observation) in observations.iter().enumerate() {
        document.push_str(&format!(
            "  - sequence: {}\n\
            \x20   kind: observation\n\
            \x20   atEpochMs: {}\n\
            \x20   status: ok\n\
            \x20   observation:\n\
            \x20     id: {}\n\
            \x20     mode: {}\n\
            \x20     topologyDigest: {}\n\
            \x20     descriptorDigest: {}\n",
            index + 1,
            observation.observed_at_epoch_ms,
            observation.observation_id,
            observation.mode,
            observation.topology_digest,
            observation.descriptor_digest,
        ));
        match &observation.serial_evidence {
            SerialEvidence::Absent => {
                document.push_str("      serialKind: absent\n");
            }
            SerialEvidence::Descriptor { digest } => {
                document.push_str(&format!(
                    "      serialKind: descriptor\n      serialDigest: {digest}\n"
                ));
            }
            SerialEvidence::ProtocolUnique { digest } => {
                document.push_str(&format!(
                    "      serialKind: protocolUnique\n      serialDigest: {digest}\n"
                ));
            }
        }
        document.push_str(&format!(
            "      identityStrength: {}\n",
            observation.identity_strength.as_str()
        ));
        if !observation.protocol_identity.is_empty() {
            document.push_str("      protocolIdentity:\n");
            for fact in &observation.protocol_identity {
                document.push_str(&format!(
                    "        - key: {}\n          value: \"{}\"\n",
                    fact.key, fact.value
                ));
            }
        }
    }

    std::fs::write(out, &document).map_err(|error| format!("{}: {error}", out.display()))?;
    // Parse it back: a capture that does not load is not a capture.
    arkforge_transport::transcript::parse(&document)
        .map_err(|error| format!("the capture does not parse: {error}"))?;
    println!("wrote {} ({} record(s))", out.display(), observations.len());
    Ok(())
}

fn capture_id(observations: &[DeviceObservation]) -> String {
    let mut hasher = arkforge_core::digest::Sha256::new();
    for observation in observations {
        hasher.update(observation.descriptor_digest.as_bytes());
        hasher.update(observation.topology_digest.as_bytes());
    }
    format!("CAP-{}", &hasher.finalize().to_hex()[..24].to_uppercase())
}

// ---------------------------------------------------------------------------
// The managed control port, host-side diagnostic implementation
// ---------------------------------------------------------------------------

/// Implements the typed control port against the host's hdc.
///
/// The typed action decides the argv; a caller cannot supply one. This is the
/// shape architecture.md 9.2 requires, implemented here for diagnostics — in
/// production the ArkDeck adapter fills this role and the HDC server stays
/// ArkDeck's.
struct HostHdcControlPort {
    hdc: PathBuf,
    target: String,
}

impl HostHdcControlPort {
    fn argv(&self, action: ManagedDeviceControlAction) -> Vec<String> {
        match action {
            // The reviewed DAYU200 transition. `hdc` forwards a non-option MODE
            // to `begetctl reboot`; `loader` is the RockUSB personality, and
            // `-bootloader` would select fastboot, which rkdeveloptool cannot
            // drive (ArkDeck `RockchipHDCIntegrationProfile.enterLoaderArguments`).
            ManagedDeviceControlAction::EnterUpdater => vec![
                "-t".into(),
                self.target.clone(),
                "target".into(),
                "boot".into(),
                "loader".into(),
            ],
            // Leaving Loader is rkdeveloptool's job, not hdc's — in Loader the
            // device has no HDC to talk to.
            ManagedDeviceControlAction::RebootToNormal => Vec::new(),
            ManagedDeviceControlAction::ReadProductFacts => vec![
                "-t".into(),
                self.target.clone(),
                "shell".into(),
                "param".into(),
                "get".into(),
                "const.product.model".into(),
            ],
            ManagedDeviceControlAction::ReadBuildFacts => vec![
                "-t".into(),
                self.target.clone(),
                "shell".into(),
                "param".into(),
                "get".into(),
                "const.ohos.fullname".into(),
            ],
        }
    }

    fn execute(
        &self,
        action: ManagedDeviceControlAction,
    ) -> Result<ManagedControlReceipt, String> {
        let argv = self.argv(action);
        if argv.is_empty() {
            return Err(format!(
                "{} has no hdc form; it is a Loader-mode action",
                action.as_str()
            ));
        }
        let output = Command::new(&self.hdc)
            .args(&argv)
            .output()
            .map_err(|error| format!("{}: {error}", self.hdc.display()))?;
        let stdout = String::from_utf8_lossy(&output.stdout)
            .trim()
            .trim_end_matches('\r')
            .to_string();
        Ok(ManagedControlReceipt {
            action,
            accepted: output.status.success(),
            facts: vec![(
                OpaqueId::new("stdout").expect("literal identifier"),
                stdout.clone(),
            )],
            evidence_digest: sha256(stdout.as_bytes()),
        })
    }
}

fn read_facts(options: &Options) -> Result<(), String> {
    let target = options
        .target
        .as_ref()
        .ok_or("facts requires --target <connect key>")?;
    let port = HostHdcControlPort {
        hdc: options.hdc.clone(),
        target: target.clone(),
    };
    println!("hdc            {}", options.hdc.display());
    println!("hdc sha256     {}", tool_digest(&options.hdc)?);
    for action in [
        ManagedDeviceControlAction::ReadProductFacts,
        ManagedDeviceControlAction::ReadBuildFacts,
    ] {
        let receipt = port.execute(action)?;
        println!(
            "{:<14} {} (accepted={}) digest={}",
            action.as_str(),
            receipt
                .facts
                .first()
                .map(|(_, value)| value.as_str())
                .unwrap_or(""),
            receipt.accepted,
            receipt.evidence_digest
        );
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// Loader-mode read-only measurements
// ---------------------------------------------------------------------------

fn rkdeveloptool(options: &Options, args: &[&str]) -> Result<(String, bool), String> {
    // Read-only commands only. `wl` and `wlx` are not reachable from here:
    // every caller in this file passes a literal.
    let output = Command::new(&options.rkdeveloptool)
        .args(args)
        .output()
        .map_err(|error| format!("{}: {error}", options.rkdeveloptool.display()))?;
    let mut text = String::from_utf8_lossy(&output.stdout).to_string();
    text.push_str(&String::from_utf8_lossy(&output.stderr));
    Ok((text, output.status.success()))
}

fn partition_table(options: &Options) -> Result<(), String> {
    println!("rkdeveloptool  {}", options.rkdeveloptool.display());
    println!("      sha256   {}", tool_digest(&options.rkdeveloptool)?);
    let (listed, _) = rkdeveloptool(options, &["ld"])?;
    println!("\n--- ld (device list) ---\n{}", listed.trim());
    let (table, ok) = rkdeveloptool(options, &["ppt"])?;
    println!("\n--- ppt (the device's own partition table) ---\n{}", table.trim());
    if !ok {
        return Err("ppt did not succeed; is the device in Loader mode?".into());
    }
    Ok(())
}

/// Measures how far the `rl` read face reaches (AD-006).
///
/// Reads one sector at a series of offsets and reports what came back. A read
/// that returns uniform filler is *not* reported as "empty" — past the window
/// the read path answers filler regardless of the medium, which is exactly the
/// confusion that produced a day of false "fake write" diagnoses.
fn read_domain(options: &Options) -> Result<(), String> {
    let directory = std::env::temp_dir().join(format!("arkforge-rd-{}", std::process::id()));
    std::fs::create_dir_all(&directory).map_err(|error| error.to_string())?;
    println!("rkdeveloptool  {}", options.rkdeveloptool.display());
    println!("      sha256   {}", tool_digest(&options.rkdeveloptool)?);
    println!("\n  {:>12}  {:>10}  {}", "sector", "bytes", "content");

    // LBA 1 is the primary GPT; the rest bracket the 65536-sector boundary the
    // 2026-08-04 session observed, and reach into the mapped partitions.
    let probes: [u64; 9] = [1, 8192, 32768, 65535, 65536, 65537, 131072, 245760, 4440064];
    let mut results = Vec::new();
    for sector in probes {
        let path = directory.join(format!("s{sector}.bin"));
        let sector_text = sector.to_string();
        let path_text = path.to_string_lossy().to_string();
        let (_, ok) = rkdeveloptool(
            options,
            &["rl", &sector_text, "1", &path_text],
        )?;
        let bytes = std::fs::read(&path).unwrap_or_default();
        let description = classify(&bytes);
        println!("  {sector:>12}  {:>10}  {description}", bytes.len());
        results.push((sector, ok, bytes.len(), description));
        let _ = std::fs::remove_file(&path);
    }
    let _ = std::fs::remove_dir_all(&directory);

    println!("\nreading note: past the read window this loader answers uniform filler");
    println!("regardless of what is on the medium (AD-006). A `uniform` row is therefore");
    println!("not evidence that the medium is blank there.");
    Ok(())
}

fn classify(bytes: &[u8]) -> String {
    if bytes.is_empty() {
        return "(no bytes returned)".to_string();
    }
    let first = bytes[0];
    if bytes.iter().all(|byte| *byte == first) {
        return format!("uniform {first:#04x}");
    }
    let printable: String = bytes
        .iter()
        .take(16)
        .map(|byte| {
            if (0x20..0x7f).contains(byte) {
                *byte as char
            } else {
                '.'
            }
        })
        .collect();
    format!("varied, sha256={} head={printable:?}", sha256(bytes))
}

// ---------------------------------------------------------------------------
// Mode changes
// ---------------------------------------------------------------------------

fn require_acknowledgement(options: &Options, what: &str) -> Result<(), String> {
    if options.mode_change_acknowledged {
        Ok(())
    } else {
        Err(format!(
            "{what} changes the device's mode. Re-run with --i-am-changing-device-mode if that is \
             what you intend. No partition is written either way."
        ))
    }
}

fn enter_loader(options: &Options) -> Result<(), String> {
    require_acknowledgement(options, "enter-loader")?;
    let target = options
        .target
        .as_ref()
        .ok_or("enter-loader requires --target <connect key>")?;
    let port = HostHdcControlPort {
        hdc: options.hdc.clone(),
        target: target.clone(),
    };
    let argv = port.argv(ManagedDeviceControlAction::EnterUpdater);
    println!("hdc sha256     {}", tool_digest(&options.hdc)?);
    println!("action         enterUpdater");
    println!("argv           {argv:?}");
    let receipt = port.execute(ManagedDeviceControlAction::EnterUpdater)?;
    println!("accepted       {}", receipt.accepted);
    println!(
        "stdout         {:?}",
        receipt
            .facts
            .first()
            .map(|(_, value)| value.as_str())
            .unwrap_or("")
    );
    println!("\nThe device should now drop off HDC and re-enumerate in Loader mode.");
    Ok(())
}

fn reboot_normal(options: &Options) -> Result<(), String> {
    require_acknowledgement(options, "reboot-normal")?;
    println!("rkdeveloptool  {}", options.rkdeveloptool.display());
    println!("      sha256   {}", tool_digest(&options.rkdeveloptool)?);
    let (text, ok) = rkdeveloptool(options, &["rd"])?;
    println!("action         resetDevice");
    println!("accepted       {ok}");
    println!("stdout         {}", text.trim());
    Ok(())
}

fn now_epoch_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|delta| delta.as_millis() as u64)
        .unwrap_or(0)
}

/// Silences the unused warning for a record type the diagnostics print via the
/// transport's own accessors.
#[allow(dead_code)]
fn _record_shape(record: &UsbDeviceRecord) -> u16 {
    record.vendor_id
}
