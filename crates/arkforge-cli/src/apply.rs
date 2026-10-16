//! `apply` command.

use super::*;

/// The general consent verb.
///
/// One command for every plan an authority sealed — a normal flash plan and a
/// superseding recovery plan alike — because what the caller is doing is the
/// same act: accepting a named set of destructive effects. Nothing here relaxes
/// the gate; the exact digest and the exact acknowledgement set are still the
/// only way through, and no broad `--yes` exists to add.
pub(crate) fn run_apply(arguments: &[String], globals: Globals) -> Result<i32, CliError> {
    let options = Options::parse(arguments)?;
    let plan_id = options.one("plan")?;
    // Rescue keeps its own consent surface on purpose. The canonical shape is
    // recognized here, before any authority store is read, so a rescue id can
    // never be resolved against the wrong domain even momentarily.
    if plan_id.starts_with(rescue::RESCUE_PLAN_PREFIX) {
        return Err(CliError::new(
            "RESCUE_PLAN_DOMAIN",
            format!(
                "{plan_id} is a rescue plan. Rescue keeps a separate plan, receipt, and evidence domain; apply it with 'arkforge rescue apply'."
            ),
            2,
            false,
        ));
    }
    let expected = options.one("expect-plan-sha256")?;
    parse_digest("--expect-plan-sha256", expected)?;
    let acknowledgements = options.many_required("ack")?.to_vec();
    let detach = options.flag("detach");
    let runtime_dir = command_runtime_dir(&globals)?;
    // Before dispatch, never after: a campaign mismatch must cost nothing.
    ensure_runtime(&globals, options.optional_one("hardware-campaign")?)?;
    let job_id =
        supervisor::apply_plan(&runtime_dir, plan_id, expected, &acknowledgements, detach)?;
    if detach {
        match globals.output {
            Output::Human => {
                println!("Started durable job {job_id}.");
                println!("The authority supervisor continues to drive it.");
                println!("Next: arkforge watch --job {job_id}");
            }
            Output::Json => emit_json!(
                "{{\"schema\":\"arkforge.apply/v1\",\"job_id\":{},\"detached\":true,\"authority_continues\":true,\"next_commands\":[{}]}}",
                json(&job_id),
                json(&format!("arkforge watch --job {job_id}")),
            ),
        }
        return Ok(0);
    }
    let (events, summary, _) = job::watch_job(&globals, &job_id, 0, u64::MAX)?;
    job::print_job_watch(&globals, &["apply"], 0, u64::MAX, &events, &summary, false);
    Ok(match summary.state.as_str() {
        "succeeded" => 0,
        "outcomeUnknown" => 8,
        "cancelledSafe" => 7,
        _ if summary.terminal => 7,
        _ => 9,
    })
}
