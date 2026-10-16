//! `artifact` command.

use super::*;

pub(crate) fn run_artifact(arguments: &[String], globals: Globals) -> Result<i32, CliError> {
    let Some(subcommand) = arguments.first() else {
        help::print_help(help::help_spec(&["artifact".into()])?, globals.output);
        return Ok(0);
    };
    let options = Options::parse(&arguments[1..])?;
    match subcommand.as_str() {
        "preboot-ramdisk" => device::prepare_preboot_ramdisk(&options, &globals),
        "import" => {
            let file = Path::new(options.one("file")?);
            let expected = options
                .optional_one("expect-sha256")?
                .map(|value| parse_digest("--expect-sha256", value))
                .transpose()?;
            let metadata = std::fs::metadata(file).map_err(|error| {
                CliError::new(
                    "ARTIFACT_FILE_NOT_FOUND",
                    format!("Cannot read artifact input {}: {error}", file.display()),
                    5,
                    false,
                )
            })?;
            if !metadata.is_file() {
                return Err(CliError::invalid(format!(
                    "--file must name one regular file, not {}.",
                    file.display()
                )));
            }
            let store = open_artifact_store(&globals)?;
            let input = File::open(file).map_err(|error| {
                CliError::new(
                    "ARTIFACT_FILE_NOT_FOUND",
                    format!("Cannot open artifact input {}: {error}", file.display()),
                    5,
                    false,
                )
            })?;
            let imported = store
                .import(input, metadata.len(), expected)
                .map_err(artifact_store_error)?;
            // Importing is the moment the caller learns what they have. Making
            // them run three more queries to find out whether it matches the
            // board on their desk is the pipework this change exists to remove.
            let registry = profile_registry()?;
            let manifest = stored_manifest(&store, &imported.digest)?;
            let response = manifest_response(&manifest);
            let compatible = registry.compatible_with_format(&response.format_id);
            let present = present_matching_devices(&globals, &registry, &compatible)?;
            print_artifact_import(globals.output, &imported, &response, &compatible, &present);
            Ok(0)
        }
        "list" => {
            let registry = profile_registry()?;
            let store_root_exists = artifact_store_root(&globals)?.exists();
            let objects = list_artifacts(&globals)?;
            let mut items = Vec::with_capacity(objects.len());
            if store_root_exists && !objects.is_empty() {
                let store = open_existing_artifact_store(&globals)?;
                for (digest, size_bytes) in &objects {
                    items.push(match stored_manifest(&store, digest) {
                        Ok(manifest) => {
                            let response = manifest_response(&manifest);
                            StoredArtifact {
                                digest: *digest,
                                size_bytes: *size_bytes,
                                compatible_profiles: registry
                                    .compatible_with_format(&response.format_id),
                                format: Some(response.format_id),
                                unreadable_reason: None,
                            }
                        }
                        Err(error) => StoredArtifact {
                            digest: *digest,
                            size_bytes: *size_bytes,
                            format: None,
                            compatible_profiles: Vec::new(),
                            unreadable_reason: Some(error.code),
                        },
                    });
                }
            }
            print_artifact_list(globals.output, &items);
            Ok(0)
        }
        "show" => {
            let artifact_id = options.one("artifact")?;
            let digest = parse_digest("--artifact", artifact_id)?;
            let registry = profile_registry()?;
            let store = open_existing_artifact_store(&globals)?;
            let manifest = stored_manifest(&store, &digest)?;
            let response = manifest_response(&manifest);
            let coverage = options
                .optional_one("profile-file")?
                .map(|path| load_profile_coverage(Path::new(path), &manifest))
                .transpose()?;
            let compatible = registry.compatible_with_format(&response.format_id);
            print_artifact_inspection(
                globals.output,
                artifact_id,
                &response,
                coverage.as_ref(),
                &compatible,
            );
            Ok(0)
        }
        other => Err(CliError::invalid(format!(
            "Unknown artifact command {other:?}. Run 'arkforge help artifact'."
        ))),
    }
}

/// One stored object as the artifact list reports it. An object whose bytes do
/// not parse keeps its place with a null format and a typed reason rather than
/// disappearing from the listing.
struct StoredArtifact {
    digest: Sha256Digest,
    size_bytes: u64,
    format: Option<String>,
    compatible_profiles: Vec<String>,
    unreadable_reason: Option<String>,
}

/// Parses one stored object with the parser its framing selects.
pub(crate) fn stored_manifest(
    store: &ContentAddressedStore,
    digest: &Sha256Digest,
) -> Result<arkforge_artifact::manifest::ArtifactManifest, CliError> {
    let object = store.open_object(digest).map_err(artifact_store_error)?;
    inspect_container(object)
        .map_err(|message| CliError::new("ARTIFACT_REJECTED", message, 3, false))
}

/// Devices currently on this host that the given profiles could flash.
///
/// A runtime that is not running makes this unknown, not empty: reporting "no
/// matching device" when nothing was enumerated would be a lie with a plausible
/// shape.
fn present_matching_devices(
    globals: &Globals,
    registry: &inference::ProfileRegistry,
    artifact_profiles: &[String],
) -> Result<status::StatusSection, CliError> {
    if artifact_profiles.is_empty() {
        return Ok(status::StatusSection::enumerated(Vec::new(), Vec::new()));
    }
    let mut client = match public_client(globals) {
        Ok(client) => client,
        Err(error) => return Ok(status::StatusSection::unobservable(error.code)),
    };
    let observations = match client.device_list() {
        Ok(observations) => observations,
        Err(error) => return Ok(status::StatusSection::unobservable(error.code)),
    };
    let mut items = Vec::new();
    let mut lines = Vec::new();
    for observation in &observations {
        let identification = inference::identify(registry, observation, None);
        if !identification
            .compatible_profiles
            .iter()
            .any(|profile| artifact_profiles.contains(profile))
        {
            continue;
        }
        items.push(format!(
            "{{\"observation_id\":{},\"mode\":{},\"identification\":{}}}",
            json(&observation.observation_id),
            json(&observation.mode),
            identification.to_json(json)
        ));
        lines.push(format!(
            "{}  mode={}  profiles={}",
            observation.observation_id,
            observation.mode,
            identification.compatible_profiles.join(", ")
        ));
    }
    Ok(status::StatusSection::enumerated(items, lines))
}
/// One import answers the whole staging question: what landed in the store,
/// what it parses as, which profiles declare that format, and which of the
/// devices on this host could take it.
fn print_artifact_import(
    output: Output,
    imported: &ImportedObject,
    manifest: &InspectArtifactResponse,
    compatible: &[String],
    present: &status::StatusSection,
) {
    let artifact_id = imported.digest.to_hex();
    let next = match (compatible.first(), present.items.as_ref()) {
        (Some(profile), Some(items)) if !items.is_empty() => format!(
            "arkforge flash plan --artifact {artifact_id} --profile {profile} --device <observation-id> --intent full-restore"
        ),
        _ => format!("arkforge artifact show --artifact {artifact_id}"),
    };
    match output {
        Output::Human => {
            println!("Artifact imported into the content-addressed store.");
            println!("artifact_id   {artifact_id}");
            println!("sha256        {artifact_id}");
            println!("size_bytes    {}", imported.size_bytes);
            println!("deduplicated  {}", imported.deduplicated);
            println!("format        {}", manifest.format_id);
            println!("confidence    {}", manifest.confidence);
            println!("members       {}", manifest.members.len());
            println!("partitions    {}", manifest.partitions.len());
            println!(
                "compatible profiles  {}",
                if compatible.is_empty() {
                    "none".to_string()
                } else {
                    compatible.join(", ")
                }
            );
            present.print_human(
                "matching devices present",
                "None of the connected devices declare a compatible profile.",
            );
            println!("No device was accessed or mutated.");
            println!("Next: {next}");
        }
        Output::Json => emit_json!(
            "{{\"schema\":\"arkforge.artifact-import/v1\",\"artifact_id\":{},\"sha256\":{},\"size_bytes\":{},\"deduplicated\":{},\"host_store_mutated\":{},\"device_accessed\":false,\"manifest_summary\":{},\"compatible_profiles\":{},\"present_devices\":{},\"next_commands\":[{}]}}",
            json(&artifact_id),
            json(&artifact_id),
            imported.size_bytes,
            imported.deduplicated,
            !imported.deduplicated,
            manifest_summary_json(manifest),
            json_strings(compatible),
            present.json(),
            json(&next)
        ),
    }
}

/// A bounded manifest projection. The full member and partition tables belong
/// to `artifact show`; repeating them in every composite document would make
/// the composites grow without bound.
pub(crate) fn manifest_summary_json(manifest: &InspectArtifactResponse) -> String {
    format!(
        "{{\"format_id\":{},\"content_sha256\":{},\"manifest_sha256\":{},\"size_bytes\":{},\"confidence\":{},\"member_count\":{},\"partition_count\":{},\"unclassified_member_count\":{},\"execution_relevant_unknown_count\":{}}}",
        json(&manifest.format_id),
        json(&manifest.content_sha256),
        json(&manifest.manifest_sha256),
        manifest.size_bytes,
        json(&manifest.confidence),
        manifest.members.len(),
        manifest.partitions.len(),
        manifest.unclassified_members.len(),
        manifest.execution_relevant_unknowns.len(),
    )
}

fn print_artifact_list(output: Output, items: &[StoredArtifact]) {
    let next = if items.is_empty() {
        "arkforge artifact import --file <firmware-file>".to_string()
    } else {
        "arkforge artifact show --artifact <artifact-id>".to_string()
    };
    match output {
        Output::Human => {
            if items.is_empty() {
                println!("No artifacts are stored in this runtime.");
                println!("Next: {next}");
                return;
            }
            println!("Stored artifacts ({})", items.len());
            for item in items {
                println!("{}  size_bytes={}", item.digest.to_hex(), item.size_bytes);
                match (&item.format, &item.unreadable_reason) {
                    (Some(format), _) => {
                        println!("  format               {format}");
                        println!(
                            "  compatible profiles  {}",
                            if item.compatible_profiles.is_empty() {
                                "none".to_string()
                            } else {
                                item.compatible_profiles.join(", ")
                            }
                        );
                    }
                    (None, Some(reason)) => {
                        println!("  format               unreadable ({reason})")
                    }
                    (None, None) => println!("  format               unreadable"),
                }
            }
            println!("Next: {next}");
        }
        Output::Json => {
            let artifacts = items
                .iter()
                .map(|item| {
                    format!(
                        "{{\"artifact_id\":{},\"sha256\":{},\"size_bytes\":{},\"format\":{},\"compatible_profiles\":{},\"unreadable_reason\":{}}}",
                        json(&item.digest.to_hex()),
                        json(&item.digest.to_hex()),
                        item.size_bytes,
                        optional_json(item.format.as_deref()),
                        json_strings(&item.compatible_profiles),
                        optional_json(item.unreadable_reason.as_deref())
                    )
                })
                .collect::<Vec<_>>()
                .join(",");
            emit_json!(
                "{{\"schema\":\"arkforge.artifact-list/v1\",\"artifacts\":[{artifacts}],\"next_commands\":[{}]}}",
                json(&next)
            );
        }
    }
}

fn print_artifact_inspection(
    output: Output,
    artifact_id: &str,
    manifest: &InspectArtifactResponse,
    coverage: Option<&ProfileCoverage>,
    compatible: &[String],
) {
    let next = match compatible.first() {
        Some(profile) => format!(
            "arkforge flash plan --artifact {artifact_id} --profile {profile} --device <observation-id> --intent full-restore"
        ),
        None => format!(
            "arkforge flash plan --artifact {artifact_id} --profile <profile-id> --device <observation-id> --intent full-restore"
        ),
    };
    match output {
        Output::Human => {
            print_artifact_human(artifact_id, manifest);
            println!(
                "compatible profiles  {}",
                if compatible.is_empty() {
                    "none".to_string()
                } else {
                    compatible.join(", ")
                }
            );
            if let Some(coverage) = coverage {
                print_profile_coverage_human(coverage);
            }
            println!("No device was accessed or mutated.");
            println!("Next: {next}");
        }
        Output::Json => emit_json!(
            "{{\"schema\":\"arkforge.artifact-inspection/v1\",{},\"compatible_profiles\":{},\"profile_coverage\":{},\"device_accessed\":false,\"next_commands\":[{}]}}",
            artifact_fields_json(artifact_id, manifest),
            json_strings(compatible),
            coverage
                .map(profile_coverage_json)
                .unwrap_or_else(|| "null".into()),
            json(&next)
        ),
    }
}

fn print_artifact_human(artifact_id: &str, manifest: &InspectArtifactResponse) {
    println!("artifact_id      {artifact_id}");
    println!("format           {}", manifest.format_id);
    println!("content_sha256   {}", manifest.content_sha256);
    println!("manifest_sha256  {}", manifest.manifest_sha256);
    println!("size_bytes       {}", manifest.size_bytes);
    println!("confidence       {}", manifest.confidence);
    println!("members ({})", manifest.members.len());
    for member in &manifest.members {
        println!(
            "  {}  size={}  role={}  sha256={}",
            member.path, member.size_bytes, member.role, member.sha256
        );
    }
    println!("partitions ({})", manifest.partitions.len());
    for partition in &manifest.partitions {
        let sectors = partition
            .size_sectors
            .map(|value| value.to_string())
            .unwrap_or_else(|| "remainder".into());
        println!(
            "  {}  index={}  start={}  sectors={}  attribute={}  grammar={}",
            partition.name,
            partition.index,
            partition.offset_sectors,
            sectors,
            partition.attribute,
            partition.grammar_branch
        );
    }
    job::print_key_values_human("build facts", &manifest.build_facts);
    if !manifest.unclassified_members.is_empty() {
        println!("unclassified members");
        for member in &manifest.unclassified_members {
            println!("  {member}");
        }
    }
    job::print_key_values_human(
        "execution-relevant unknowns",
        &manifest.execution_relevant_unknowns,
    );
}

fn print_profile_coverage_human(coverage: &ProfileCoverage) {
    println!(
        "profile coverage  {} {} sha256={} complete={}",
        coverage.profile_id, coverage.profile_version, coverage.profile_sha256, coverage.complete
    );
    for target in &coverage.targets {
        println!(
            "  order={} partition={} source={} size_bytes={} present={}",
            target.write_order,
            target.partition,
            target.source_member.as_deref().unwrap_or("none"),
            target
                .source_size_bytes
                .map(|value| value.to_string())
                .unwrap_or_else(|| "null".into()),
            target.present
        );
    }
}

fn artifact_fields_json(artifact_id: &str, manifest: &InspectArtifactResponse) -> String {
    let members = manifest
        .members
        .iter()
        .map(|member| {
            format!(
                "{{\"path\":{},\"size_bytes\":{},\"sha256\":{},\"role\":{}}}",
                json(&member.path),
                member.size_bytes,
                json(&member.sha256),
                json(&member.role)
            )
        })
        .collect::<Vec<_>>()
        .join(",");
    let partitions = manifest
        .partitions
        .iter()
        .map(|partition| {
            format!(
                "{{\"index\":{},\"name\":{},\"offset_sectors\":{},\"size_sectors\":{},\"attribute\":{},\"grammar_branch\":{}}}",
                partition.index,
                json(&partition.name),
                partition.offset_sectors,
                optional_u64(partition.size_sectors),
                json(&partition.attribute),
                json(&partition.grammar_branch)
            )
        })
        .collect::<Vec<_>>()
        .join(",");
    format!(
        "\"artifact_id\":{},\"format_id\":{},\"content_sha256\":{},\"manifest_sha256\":{},\"size_bytes\":{},\"confidence\":{},\"members\":[{}],\"partitions\":[{}],\"build_facts\":{},\"unclassified_members\":{},\"execution_relevant_unknowns\":{}",
        json(artifact_id),
        json(&manifest.format_id),
        json(&manifest.content_sha256),
        json(&manifest.manifest_sha256),
        manifest.size_bytes,
        json(&manifest.confidence),
        members,
        partitions,
        job::key_values_json(&manifest.build_facts),
        json_array(&manifest.unclassified_members),
        job::key_values_json(&manifest.execution_relevant_unknowns)
    )
}

fn profile_coverage_json(coverage: &ProfileCoverage) -> String {
    let targets = coverage
        .targets
        .iter()
        .map(|target| {
            format!(
                "{{\"write_order\":{},\"partition\":{},\"source_member\":{},\"source_size_bytes\":{},\"present\":{}}}",
                target.write_order,
                json(&target.partition),
                optional_json(target.source_member.as_deref()),
                optional_u64(target.source_size_bytes),
                target.present
            )
        })
        .collect::<Vec<_>>()
        .join(",");
    format!(
        "{{\"profile_id\":{},\"profile_version\":{},\"profile_sha256\":{},\"complete\":{},\"targets\":[{}]}}",
        json(&coverage.profile_id),
        json(&coverage.profile_version),
        json(&coverage.profile_sha256),
        coverage.complete,
        targets
    )
}
