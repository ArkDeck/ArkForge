//! `daemon` command.

use super::*;

pub(crate) fn run_daemon(arguments: &[String], globals: Globals) -> Result<i32, CliError> {
    let Some(subcommand) = arguments.first() else {
        help::print_help(help::help_spec(&["daemon".into()])?, globals.output);
        return Ok(0);
    };
    let runtime_dir = command_runtime_dir(&globals)?;
    match subcommand.as_str() {
        "run" => {
            let options = daemon_options(&runtime_dir, &arguments[1..])?;
            supervisor::run(
                runtime_dir,
                options,
                globals.output == Output::Human && !globals.quiet,
            )?;
            Ok(0)
        }
        "start" => {
            let options = daemon_options(&runtime_dir, &arguments[1..])?;
            let status = supervisor::start(runtime_dir, options)?;
            print_daemon_status(&status, globals.output);
            Ok(0)
        }
        "stop" => {
            reject_extra(&arguments[1..], "daemon stop")?;
            let status = supervisor::stop(&runtime_dir)?;
            match globals.output {
                Output::Human => {
                    println!("ArkForge runtime stopped (epoch {}).", status.epoch);
                    println!("Next: arkforge daemon start");
                }
                Output::Json => emit_json!(
                    "{{\"schema\":\"arkforge.daemon-stop/v1\",\"stopped\":true,\"pairing_epoch\":{},\"next_commands\":[\"arkforge daemon start\"]}}",
                    status.epoch
                ),
            }
            Ok(0)
        }
        other => Err(CliError::invalid(format!(
            "Unknown daemon command {other:?}. Run 'arkforge help daemon'."
        ))),
    }
}

/// The lifecycle options for this run: stored configuration, with explicit
/// arguments overriding it wherever they give a complete binding.
fn daemon_options(
    runtime_dir: &Path,
    arguments: &[String],
) -> Result<supervisor::DaemonOptions, CliError> {
    let config = RuntimeConfig::load(runtime_dir)?;
    config.verify_pins()?;
    Ok(supervisor::DaemonOptions::from_config(&config, None)
        .overridden_by(supervisor::DaemonOptions::parse(arguments)?))
}

fn print_daemon_status(status: &supervisor::DaemonStatus, output: Output) {
    let next = daemon_next_commands(status);
    match output {
        Output::Human => {
            println!("ArkForge runtime: running");
            println!("  supervisor pid: {}", status.supervisor_pid);
            println!("  mechanics pid: {}", status.daemon_pid);
            println!(
                "  protocol: {}.{}",
                status.protocol_major, status.protocol_minor
            );
            println!("  daemon: {}", status.daemon_version);
            println!("  authority: arkforge.cli (epoch {})", status.epoch);
            println!("  mechanics ready: {}", status.mechanics_ready);
            println!(
                "  authority support available: {}",
                status.authority_support_available
            );
            println!("  HDC bound: {}", status.hdc_bound);
            if status.hdc_bound {
                println!("  HDC SHA-256: {}", status.hdc_sha256);
            }
            if !status.hardware_campaign.is_empty() {
                println!("  hardware campaign: {}", status.hardware_campaign);
            }
            println!("  active jobs: {}", status.active_jobs);
            if !status.blockers.is_empty() {
                println!("  blockers: {}", status.blockers.join(", "));
            }
            println!("Next: {}", next[0]);
        }
        Output::Json => emit_json!(
            "{{\"schema\":\"arkforge.daemon-status/v1\",\"running\":true,\"supervisor_pid\":{},\"daemon_pid\":{},\"protocol\":{{\"major\":{},\"minor\":{}}},\"daemon_version\":{},\"authority\":{{\"namespace\":\"arkforge.cli\",\"pairing_epoch\":{},\"support_records_available\":{},\"hardware_campaign\":{},\"hdc\":{{\"bound\":{},\"sha256\":{}}}}},\"mechanics_ready\":{},\"active_jobs\":{},\"blockers\":{},\"next_commands\":{}}}",
            status.supervisor_pid,
            status.daemon_pid,
            status.protocol_major,
            status.protocol_minor,
            json(&status.daemon_version),
            status.epoch,
            status.authority_support_available,
            optional_json(
                (!status.hardware_campaign.is_empty()).then_some(status.hardware_campaign.as_str())
            ),
            status.hdc_bound,
            optional_json(status.hdc_bound.then_some(status.hdc_sha256.as_str())),
            status.mechanics_ready,
            status.active_jobs,
            json_strings(&status.blockers),
            json_strings(&next),
        ),
    }
}

pub(crate) fn daemon_next_commands(status: &supervisor::DaemonStatus) -> Vec<String> {
    if status.active_jobs > 0 {
        return vec!["arkforge job list".into()];
    }
    if !status.hdc_bound || !status.authority_support_available {
        return vec![
            "arkforge daemon stop".into(),
            "arkforge help daemon start --format json".into(),
        ];
    }
    if status.mechanics_ready && status.blockers.is_empty() {
        vec!["arkforge device list".into()]
    } else {
        vec!["arkforge status".into()]
    }
}
