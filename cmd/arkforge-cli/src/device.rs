//! `device` command.

use super::*;

pub(crate) fn run_device(arguments: &[String], globals: Globals) -> Result<i32, CliError> {
    let Some(subcommand) = arguments.first() else {
        help::print_help(help::help_spec(&["device".into()])?, globals.output);
        return Ok(0);
    };
    let options = Options::parse(&arguments[1..])?;
    match subcommand.as_str() {
        "list" => {
            let selected = options.optional_one("device")?.map(str::to_string);
            let deep = options.flag("deep");
            ensure_runtime(&globals, options.optional_one("hardware-campaign")?)?;
            let registry = profile_registry()?;
            let mut client = public_client(&globals)?;
            let mut observations = client.device_list()?;
            if let Some(selected) = &selected {
                observations.retain(|observation| observation.observation_id == *selected);
                if observations.is_empty() {
                    return Err(CliError::new(
                        "OBSERVATION_NOT_FOUND",
                        format!("No current observation has id {selected}."),
                        5,
                        false,
                    ));
                }
            }
            let mut entries = Vec::with_capacity(observations.len());
            for observation in &observations {
                entries.push(describe_device(&mut client, &registry, observation, deep)?);
            }
            print_device_list(globals.output, deep, selected.as_deref(), &entries);
            Ok(0)
        }
        "wait" => {
            let profile = options.one("profile")?;
            let mode = options.one("mode")?;
            if mode.trim().is_empty() {
                return Err(CliError::invalid("--mode cannot be empty."));
            }
            let timeout_ms = options
                .optional_one("timeout-ms")?
                .map(|value| parse_u64("--timeout-ms", value))
                .transpose()?
                .unwrap_or(30_000);
            ensure_runtime(&globals, options.optional_one("hardware-campaign")?)?;
            let probe = wait_for_device(&globals, profile, mode, timeout_ms)?;
            print_device_wait(globals.output, profile, mode, timeout_ms, &probe);
            Ok(0)
        }
        other => Err(CliError::invalid(format!(
            "Unknown device command {other:?}. Run 'arkforge help device'."
        ))),
    }
}
/// One observation with everything this build can say about it.
struct DeviceEntry {
    observation: DeviceObservationView,
    identification: inference::Identification,
    /// Facts an active probe returned, or `None` when no probe ran. An empty
    /// vector would claim a probe answered with nothing, which is a different
    /// fact from not having asked.
    probe_facts: Option<Vec<(String, String)>>,
    probe_refusals: Vec<(String, String)>,
}

/// Identifies one observation, optionally confirming it on the wire.
///
/// A deep pass probes every profile the passive evidence says the device could
/// be — not one guessed profile — so an ambiguous device produces evidence for
/// each candidate instead of a conclusion for one.
fn describe_device(
    client: &mut PublicClient,
    registry: &inference::ProfileRegistry,
    observation: &DeviceObservationView,
    deep: bool,
) -> Result<DeviceEntry, CliError> {
    let passive = inference::identify(registry, observation, None);
    if !deep {
        return Ok(DeviceEntry {
            observation: observation.clone(),
            identification: passive,
            probe_facts: None,
            probe_refusals: Vec::new(),
        });
    }

    let mut facts: Vec<(String, String)> = Vec::new();
    let mut refusals = Vec::new();
    let mut answered = false;
    for profile in &passive.compatible_profiles {
        match client.device_probe(&observation.observation_id, profile) {
            Ok(probe) => {
                answered = true;
                facts.extend(
                    probe
                        .protocol_facts
                        .iter()
                        .map(|fact| (fact.key.clone(), fact.value.clone())),
                );
            }
            Err(error)
                if matches!(
                    error.code.as_str(),
                    "PROBE_REFUSED" | "OBSERVATION_NOT_FOUND" | "NO_PROVIDER_FOR_PROFILE"
                ) =>
            {
                refusals.push((profile.clone(), error.code.clone()));
            }
            Err(error) => return Err(error.into()),
        }
    }
    facts.sort();
    facts.dedup();
    let identification = if answered {
        let borrowed = facts
            .iter()
            .map(|(key, value)| (key.as_str(), value.as_str()))
            .collect::<Vec<_>>();
        inference::identify(registry, observation, Some(&borrowed))
    } else {
        passive
    };
    Ok(DeviceEntry {
        observation: observation.clone(),
        identification,
        probe_facts: answered.then_some(facts),
        probe_refusals: refusals,
    })
}

fn wait_for_device(
    globals: &Globals,
    profile: &str,
    mode: &str,
    timeout_ms: u64,
) -> Result<DeviceProbeView, CliError> {
    let started = std::time::Instant::now();
    let mut client = public_client(globals)?;
    loop {
        let observations = client.device_list()?;
        let mut matches = Vec::new();
        for observation in observations
            .iter()
            .filter(|observation| observation.mode == mode)
        {
            match client.device_probe(&observation.observation_id, profile) {
                Ok(probe) => matches.push(probe),
                Err(error)
                    if matches!(
                        error.code.as_str(),
                        "PROBE_REFUSED" | "OBSERVATION_NOT_FOUND"
                    ) => {}
                Err(error) => return Err(error.into()),
            }
        }
        match matches.len() {
            1 => return Ok(matches.remove(0)),
            count if count > 1 => {
                return Err(CliError::new(
                    "AMBIGUOUS_DEVICE",
                    format!(
                        "{count} observations match profile {profile} in mode {mode}; an exact target cannot be selected."
                    ),
                    6,
                    true,
                ));
            }
            _ => {}
        }
        if started.elapsed().as_millis() as u64 >= timeout_ms {
            return Err(CliError::new(
                "DEVICE_WAIT_TIMEOUT",
                format!(
                    "No unique observation matched profile {profile} in mode {mode} within {timeout_ms} ms."
                ),
                5,
                true,
            ));
        }
        std::thread::sleep(std::time::Duration::from_millis(100));
    }
}

pub(crate) fn prepare_preboot_ramdisk(
    options: &Options,
    globals: &Globals,
) -> Result<i32, CliError> {
    use arkforge_artifact::preboot;
    use std::io::{Read, Write};
    let fail = |e: &dyn std::fmt::Display| {
        CliError::new("PREBOOT_ARTIFACT_FAILED", e.to_string(), 3, false)
    };
    let input = std::fs::File::open(options.one("file")?).map_err(|e| fail(&e))?;
    let mut base = Vec::new();
    input
        .take(preboot::ORIGINAL_SIZE as u64 + 1)
        .read_to_end(&mut base)
        .map_err(|e| fail(&e))?;
    let out = options.one("out")?;
    let restore_out = options.one("restore-out")?;
    let mut support = Vec::new();
    std::fs::File::open(options.one("support-archive")?)
        .map_err(|e| fail(&e))?
        .take(preboot::SUPPORT_SIZE as u64 + 1)
        .read_to_end(&mut support)
        .map_err(|e| fail(&e))?;
    let prepared =
        arkforge_standalone::preboot::prepare(&command_runtime_dir(globals)?, &base, &support)?;
    for (path, bytes) in [(out, &prepared.image), (restore_out, &prepared.restoration)] {
        let mut output = std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(path)
            .map_err(|e| fail(&e))?;
        output
            .write_all(bytes)
            .and_then(|_| output.sync_all())
            .map_err(|e| fail(&e))?;
    }
    emit_json!(
        "{{\"schema\":\"arkforge.preboot-ramdisk/v1\",\"ramdisk_sha256\":{},\"original_ramdisk_sha256\":{},\"restoration_ramdisk_sha256\":{},\"support_archive_sha256\":{},\"support_library_sha256\":{},\"nonce\":{},\"size_bytes\":{},\"device_accessed\":false}}",
        json(&prepared.digest),
        json(preboot::ORIGINAL_SHA256),
        json(preboot::RESTORATION_SHA256),
        json(preboot::SUPPORT_SHA256),
        json(preboot::SUPPORT_LIBRARY_SHA256),
        json(&prepared.nonce),
        prepared.image.len()
    );
    Ok(0)
}
/// The one device query surface: enumeration, single-device filtering, and
/// active confirmation are the same answer at three depths, not three commands
/// whose outputs a caller has to reconcile.
fn print_device_list(output: Output, deep: bool, selected: Option<&str>, entries: &[DeviceEntry]) {
    let next = entries
        .iter()
        .find(|entry| entry.identification.profile.is_some())
        .map(|entry| {
            format!(
                "arkforge flash plan --artifact <artifact-id> --device {} --intent full-restore",
                entry.observation.observation_id
            )
        })
        .unwrap_or_else(|| {
            if deep {
                "arkforge status".to_string()
            } else {
                "arkforge device list --deep".to_string()
            }
        });
    match output {
        Output::Human => {
            if entries.is_empty() {
                println!("No device observations are available.");
                println!(
                    "Connect a supported device and make sure the ArkForge runtime is running."
                );
                println!("Next: arkforge device list");
                return;
            }
            println!("Device observations ({})", entries.len());
            for entry in entries {
                let identification = &entry.identification;
                println!(
                    "{}  mode={}  identity={}",
                    entry.observation.observation_id,
                    entry.observation.mode,
                    entry.observation.identity_strength
                );
                println!(
                    "  model                {}",
                    identification.model.as_deref().unwrap_or("unproven")
                );
                println!(
                    "  compatible profiles  {}",
                    if identification.compatible_profiles.is_empty() {
                        "none".to_string()
                    } else {
                        identification.compatible_profiles.join(", ")
                    }
                );
                println!(
                    "  resolution           {}",
                    identification.profile_resolution
                );
                println!(
                    "  strength             {}",
                    identification.strength.as_str()
                );
                println!(
                    "  evidence             {}",
                    identification.evidence.join(", ")
                );
                if entry.observation.malformed_descriptor {
                    println!("  descriptor           malformed");
                }
                if let Some(facts) = &entry.probe_facts {
                    println!("  probe facts ({})", facts.len());
                    for (key, value) in facts {
                        println!("    {key} = {value}");
                    }
                }
                for (profile, code) in &entry.probe_refusals {
                    println!("  probe refused        {profile}: {code}");
                }
            }
            println!("Next: {next}");
        }
        Output::Json => {
            let values = entries
                .iter()
                .map(device_entry_json)
                .collect::<Vec<_>>()
                .join(",");
            emit_json!(
                "{{\"schema\":\"arkforge.device-list/v1\",\"deep\":{deep},\"filtered_to\":{},\"observations\":[{values}],\"next_commands\":[{}]}}",
                optional_json(selected),
                json(&next)
            );
        }
    }
}

fn device_entry_json(entry: &DeviceEntry) -> String {
    let probe = match &entry.probe_facts {
        None => "null".to_string(),
        Some(facts) => {
            let facts = facts
                .iter()
                .map(|(key, value)| format!("{{\"key\":{},\"value\":{}}}", json(key), json(value)))
                .collect::<Vec<_>>()
                .join(",");
            let refusals = entry
                .probe_refusals
                .iter()
                .map(|(profile, code)| {
                    format!("{{\"profile\":{},\"code\":{}}}", json(profile), json(code))
                })
                .collect::<Vec<_>>()
                .join(",");
            format!("{{\"facts\":[{facts}],\"refusals\":[{refusals}]}}")
        }
    };
    format!(
        "{{{},\"identification\":{},\"probe\":{}}}",
        job::observation_fields_json(&entry.observation),
        entry.identification.to_json(json),
        probe
    )
}

fn print_device_wait(
    output: Output,
    requested_profile: &str,
    requested_mode: &str,
    timeout_ms: u64,
    probe: &DeviceProbeView,
) {
    let next = format!(
        "arkforge flash plan --file <firmware-file> --profile {} --device {}",
        probe.profile_id, probe.observation.observation_id
    );
    match output {
        Output::Human => {
            println!("Unique matching device observed.");
            println!("requested_profile {}", requested_profile);
            println!("requested_mode    {}", requested_mode);
            println!("timeout_ms        {timeout_ms}");
            println!("device            {}", probe.observation.observation_id);
            println!("facts_sha256      {}", probe.facts_sha256);
            println!("Next: {next}");
        }
        Output::Json => emit_json!(
            "{{\"schema\":\"arkforge.device-wait/v1\",\"requested_profile\":{},\"requested_mode\":{},\"timeout_ms\":{},\"match\":{{\"observation\":{},\"profile_id\":{},\"facts_sha256\":{},\"protocol_facts\":{}}},\"next_commands\":[{}]}}",
            json(requested_profile),
            json(requested_mode),
            timeout_ms,
            job::observation_json(&probe.observation),
            json(&probe.profile_id),
            json(&probe.facts_sha256),
            job::key_values_json(&probe.protocol_facts),
            json(&next)
        ),
    }
}
