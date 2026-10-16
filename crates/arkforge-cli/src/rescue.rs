//! `rescue` command.

use super::*;

/// The canonical rescue plan identifier shape.
pub(crate) const RESCUE_PLAN_PREFIX: &str = "rescue-plan:";

pub(crate) fn run_rescue(arguments: &[String], globals: Globals) -> Result<i32, CliError> {
    let Some(subcommand) = arguments.first() else {
        help::print_help(help::help_spec(&["rescue".into()])?, globals.output);
        return Ok(0);
    };
    let options = Options::parse(&arguments[1..])?;
    let runtime_dir = match globals.runtime_dir {
        Some(path) => path,
        None => default_runtime_dir()?,
    };
    let executable = std::env::current_exe().map_err(|error| {
        CliError::invalid(format!("Cannot locate this arkforge build: {error}"))
    })?;
    let build_digest = executable_digest(&executable).map_err(|message| {
        CliError::invalid(format!("Cannot hash this arkforge build: {message}"))
    })?;
    let manager = RescueManager::new(runtime_dir, build_digest, NativeRescueBackend::new())?;

    match subcommand.as_str() {
        "list" => {
            print_devices(globals.output, &manager.list_devices()?);
            Ok(0)
        }
        "inspect" => {
            let result = manager.inspect(options.one("device")?)?;
            print_inspection(globals.output, &result);
            Ok(0)
        }
        "read" => {
            let start = parse_u64("--start-sector", options.one("start-sector")?)?;
            let count = parse_u64("--sector-count", options.one("sector-count")?)?;
            let result = manager.read_sectors(
                options.one("device")?,
                start,
                count,
                Path::new(options.one("out")?),
            )?;
            print_read(globals.output, &result);
            Ok(0)
        }
        "plan" => {
            let now = now_epoch_ms()?;
            let result = match options.one("operation")? {
                "write-partition" => manager.plan_write(
                    options.one("device")?,
                    options.one("partition")?,
                    Path::new(options.one("image")?),
                    parse_digest("--expect-image-sha256", options.one("expect-image-sha256")?)?,
                    now,
                )?,
                "reset-device" => {
                    options.ensure_absent(&["partition", "image", "expect-image-sha256"])?;
                    manager.plan_reset(options.one("device")?, now)?
                }
                operation => {
                    return Err(CliError::invalid(format!(
                        "--operation accepts 'write-partition' or 'reset-device', not {operation:?}."
                    )));
                }
            };
            print_plan(globals.output, &result);
            Ok(0)
        }
        "apply" => {
            let result = manager.apply(
                options.one("plan")?,
                parse_digest("--expect-plan-sha256", options.one("expect-plan-sha256")?)?,
                options.many_required("ack")?,
                now_epoch_ms()?,
            )?;
            let exit = result.exit_code();
            print_apply(globals.output, &result)?;
            Ok(exit)
        }
        other => Err(CliError::invalid(format!(
            "Unknown rescue command {other:?}. Run 'arkforge help rescue'."
        ))),
    }
}
fn print_devices(output: Output, devices: &[RescueDevice]) {
    match output {
        Output::Human => {
            if devices.is_empty() {
                println!("No DAYU200 Loader devices found.");
                println!("Next: Put one device in Loader mode, then run 'arkforge rescue list'.");
            } else {
                println!("Native Loader devices ({})", devices.len());
                for device in devices {
                    println!(
                        "{}  {:04x}:{:04x}  location=0x{:08x}  mode={}",
                        device.device_id,
                        device.vendor_id,
                        device.product_id,
                        device.location_id,
                        device.mode
                    );
                }
                println!("Next: arkforge rescue inspect --device <device-id>");
            }
        }
        Output::Json => {
            let values = devices
                .iter()
                .map(device_json)
                .collect::<Vec<_>>()
                .join(",");
            emit_json!(
                "{{\"schema\":\"arkforge.rescue-device-list/v1\",\"devices\":[{values}],\"next_commands\":[\"arkforge rescue inspect --device <device-id>\"]}}"
            );
        }
    }
}

fn print_inspection(output: Output, result: &RescueInspection) {
    match output {
        Output::Human => {
            println!("device              {}", result.device.device_id);
            println!("capacity_sectors    {}", result.capacity_sectors);
            println!("layout_sha256       {}", result.layout_digest);
            println!("profile_compatible  {}", result.profile_compatible);
            if let Some(blocker) = &result.profile_blocker {
                println!("blocker             {blocker}");
            }
            println!("partitions");
            for entry in &result.table.entries {
                let size = entry
                    .size_sectors
                    .map(|value| value.to_string())
                    .unwrap_or_else(|| "remainder".into());
                println!(
                    "  {:<16} start={} sectors={size}",
                    entry.name, entry.offset_sectors
                );
            }
            println!("Next: Create a write or reset plan with 'arkforge rescue plan --help'.");
        }
        Output::Json => {
            let partitions = result
                .table
                .entries
                .iter()
                .map(|entry| {
                    format!(
                        "{{\"name\":{},\"start_sector\":{},\"sector_count\":{}}}",
                        json(&entry.name),
                        entry.offset_sectors,
                        entry
                            .size_sectors
                            .map(|value| value.to_string())
                            .unwrap_or_else(|| "null".into())
                    )
                })
                .collect::<Vec<_>>()
                .join(",");
            emit_json!(
                "{{\"schema\":\"arkforge.rescue-inspection/v1\",\"device\":{},\"capacity_sectors\":{},\"capacity_evidence_sha256\":{},\"layout_sha256\":{},\"layout_evidence_sha256\":{},\"profile_compatible\":{},\"profile_blocker\":{},\"partitions\":[{}],\"next_commands\":[\"arkforge help rescue plan --format json\"]}}",
                device_json(&result.device),
                result.capacity_sectors,
                json(&result.capacity_evidence_digest.to_string()),
                json(&result.layout_digest.to_string()),
                json(&result.layout_evidence_digest.to_string()),
                result.profile_compatible,
                optional_json(result.profile_blocker.as_deref()),
                partitions
            );
        }
    }
}

fn print_read(output: Output, result: &RescueReadReceipt) {
    match output {
        Output::Human => {
            println!(
                "Read {} bytes to {}.",
                result.bytes,
                result.output.display()
            );
            println!("sha256  {}", result.sha256);
            println!("The device was not mutated.");
        }
        Output::Json => emit_json!(
            "{{\"schema\":\"arkforge.rescue-read-receipt/v1\",\"device_id\":{},\"start_sector\":{},\"sector_count\":{},\"bytes\":{},\"sha256\":{},\"output_written\":true,\"device_mutated\":false,\"next_commands\":[{}]}}",
            json(&result.device.device_id),
            result.begin_sector,
            result.sector_count,
            result.bytes,
            json(&result.sha256.to_string()),
            json(&format!(
                "arkforge rescue inspect --device {}",
                result.device.device_id
            ))
        ),
    }
}

fn print_plan(output: Output, result: &RescuePlanSummary) {
    let acknowledgements = result.plan.required_acknowledgements();
    match output {
        Output::Human => {
            println!("Rescue plan created. The device has not been mutated.");
            println!("plan_id             {}", result.plan_id);
            println!("plan_sha256         {}", result.plan_sha256);
            println!("operation           {}", result.plan.operation.as_str());
            println!("expires_at_epoch_ms {}", result.plan.expires_at_epoch_ms);
            println!("required acknowledgements");
            for acknowledgement in &acknowledgements {
                println!("  {acknowledgement}");
            }
            print!(
                "Next: arkforge rescue apply --plan {} --expect-plan-sha256 {}",
                result.plan_id, result.plan_sha256
            );
            for acknowledgement in &acknowledgements {
                print!(" --ack {acknowledgement}");
            }
            println!();
        }
        Output::Json => {
            let acknowledgements_json = json_array(&acknowledgements);
            let next = format!(
                "arkforge rescue apply --plan {} --expect-plan-sha256 {}{}",
                result.plan_id,
                result.plan_sha256,
                acknowledgements
                    .iter()
                    .map(|value| format!(" --ack {value}"))
                    .collect::<String>()
            );
            emit_json!(
                "{{\"schema\":\"arkforge.rescue-plan-summary/v1\",\"plan_id\":{},\"plan_sha256\":{},\"device_id\":{},\"operation\":{},\"expires_at_epoch_ms\":{},\"required_acknowledgements\":{},\"device_mutated\":false,\"next_commands\":[{}]}}",
                json(&result.plan_id),
                json(&result.plan_sha256.to_string()),
                json(&result.plan.device_id),
                json(result.plan.operation.as_str()),
                result.plan.expires_at_epoch_ms,
                acknowledgements_json,
                json(&next)
            );
        }
    }
}

fn print_apply(output: Output, result: &RescueApplyResult) -> Result<(), CliError> {
    let receipt = &result.receipt;
    let receipt_digest = receipt.digest()?;
    let receipt_id = format!("rescue-receipt:{receipt_digest}");
    match output {
        Output::Human => {
            println!("Rescue result: {}", receipt.disposition.as_str());
            println!("receipt_id      {receipt_id}");
            println!("receipt_sha256  {receipt_digest}");
            println!("plan_id         {}", receipt.plan_id);
            println!("operation       {}", receipt.operation);
            println!("evidence_sha256 {}", receipt.evidence_digest);
            println!("detail          {}", receipt.detail);
            if receipt.disposition.as_str() == "outcome-unknown" {
                println!(
                    "Next: Do not replay this plan. Inspect and reconcile the device manually."
                );
            }
        }
        Output::Json => emit_json!(
            "{{\"schema\":\"arkforge.rescue-receipt/v1\",\"receipt_id\":{},\"receipt_sha256\":{},\"plan_id\":{},\"plan_sha256\":{},\"device_id\":{},\"operation\":{},\"disposition\":{},\"evidence_sha256\":{},\"completed_at_epoch_ms\":{},\"detail\":{},\"payload_bytes\":{},\"payload_sha256\":{},\"replay_allowed\":false,\"next_commands\":{}}}",
            json(&receipt_id),
            json(&receipt_digest.to_string()),
            json(&receipt.plan_id),
            json(&receipt.plan_digest.to_string()),
            json(&receipt.device_id),
            json(&receipt.operation),
            json(receipt.disposition.as_str()),
            json(&receipt.evidence_digest.to_string()),
            receipt.completed_at_epoch_ms,
            json(&receipt.detail),
            optional_u64(receipt.payload_bytes),
            receipt
                .payload_digest
                .map(|value| json(&value.to_string()))
                .unwrap_or_else(|| "null".into()),
            if receipt.disposition.as_str() == "outcome-unknown" {
                "[\"arkforge rescue inspect --device <device-id>\"]"
            } else {
                "[]"
            }
        ),
    }
    Ok(())
}

fn device_json(device: &RescueDevice) -> String {
    format!(
        "{{\"device_id\":{},\"facts_sha256\":{},\"usb_vendor_id\":{},\"usb_product_id\":{},\"usb_location_id\":{},\"mode\":{},\"serial_present\":{}}}",
        json(&device.device_id),
        json(&device.facts_digest.to_string()),
        device.vendor_id,
        device.product_id,
        device.location_id,
        json(&device.mode),
        device.serial_present
    )
}
