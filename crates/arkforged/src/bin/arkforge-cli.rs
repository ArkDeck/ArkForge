//! `arkforge-cli` — read-only diagnostics.
//!
//! architecture.md 15.1/23.4: the CLI is for read-only and offline diagnostics.
//! It connects to the **public** socket, so it cannot import an artifact and
//! cannot start execution even if a future version tried to.

use arkforge_ipc::framing::{read_frame, write_frame};
use arkforge_ipc::messages::{
    ErrorBody, Hello, HelloAck, InspectArtifactResponse, JobSummary, MaterializePlanResponse,
    Request, Response,
};
use arkforge_ipc::{Api, PROTOCOL_MAJOR, PROTOCOL_MINOR, SessionKind, Status, wire};
use std::os::unix::net::UnixStream;

fn main() {
    let arguments: Vec<String> = std::env::args().skip(1).collect();
    match run(&arguments) {
        Ok(()) => {}
        Err(message) => {
            eprintln!("arkforge-cli: {message}");
            std::process::exit(1);
        }
    }
}

fn usage() -> String {
    concat!(
        "usage: arkforge-cli --socket <public.sock> <command>\n",
        "\n",
        "commands:\n",
        "  discover                 list observed devices\n",
        "  inspect <artifact-id>    show an artifact manifest\n",
        "  assess <artifact-id> <profile-id> <observation-id>\n",
        "                           materialize a plan assessment\n",
        "  jobs                     list durable job status\n",
        "  job <job-id>             show one durable job status\n",
        "  recovery-guide <job-id>  show the no-replay recovery guide\n",
        "\n",
        "This CLI talks to the public socket: it cannot import or execute.\n"
    )
    .to_string()
}

fn run(arguments: &[String]) -> Result<(), String> {
    let mut socket = None;
    let mut rest: Vec<&str> = Vec::new();
    let mut index = 0usize;
    while index < arguments.len() {
        match arguments[index].as_str() {
            "--socket" => {
                index += 1;
                socket = Some(arguments.get(index).ok_or_else(usage)?.clone());
            }
            "--help" | "-h" => {
                print!("{}", usage());
                return Ok(());
            }
            other => rest.push(other),
        }
        index += 1;
    }
    let socket = socket.ok_or_else(usage)?;
    let command = rest.first().copied().ok_or_else(usage)?;

    let mut stream = UnixStream::connect(&socket).map_err(|error| format!("{socket}: {error}"))?;
    handshake(&mut stream)?;

    let (api, payload) = match command {
        "discover" => (Api::DiscoverDevices, Vec::new()),
        "inspect" => {
            let artifact = rest.get(1).ok_or_else(usage)?;
            let mut payload = Vec::new();
            wire::write_string(&mut payload, 1, artifact);
            (Api::InspectArtifact, payload)
        }
        "assess" => {
            let artifact = rest.get(1).ok_or_else(usage)?;
            let profile = rest.get(2).ok_or_else(usage)?;
            let observation = rest.get(3).ok_or_else(usage)?;
            let mut payload = Vec::new();
            wire::write_string(&mut payload, 1, artifact);
            wire::write_string(&mut payload, 2, profile);
            wire::write_string(&mut payload, 3, observation);
            (Api::MaterializePlan, payload)
        }
        "jobs" => (Api::ListJobs, Vec::new()),
        "job" => {
            let job_id = rest.get(1).ok_or_else(usage)?;
            let mut payload = Vec::new();
            wire::write_string(&mut payload, 1, job_id);
            (Api::GetJob, payload)
        }
        "recovery-guide" => {
            let job_id = rest.get(1).ok_or_else(usage)?;
            let mut payload = Vec::new();
            wire::write_string(&mut payload, 1, job_id);
            (Api::GetRecoveryGuide, payload)
        }
        other => return Err(format!("unknown command {other:?}\n\n{}", usage())),
    };

    let request = Request {
        request_id: "cli-1".into(),
        api,
        payload,
    };
    write_frame(&mut stream, &request.encode()).map_err(|error| error.to_string())?;
    let frame = read_frame(&mut stream)
        .map_err(|error| error.to_string())?
        .ok_or("daemon closed the connection")?;
    let response = Response::decode(&frame).map_err(|error| error.to_string())?;
    render(&response)
}

fn handshake(stream: &mut UnixStream) -> Result<(), String> {
    let hello = Hello {
        protocol_major: PROTOCOL_MAJOR,
        protocol_minor: PROTOCOL_MINOR,
        session_kind: SessionKind::Public,
    };
    write_frame(stream, &hello.encode()).map_err(|error| error.to_string())?;
    let frame = read_frame(stream)
        .map_err(|error| error.to_string())?
        .ok_or("daemon closed during handshake")?;
    let ack = HelloAck::decode(&frame).map_err(|error| error.to_string())?;
    match ack.refusal {
        Some(refusal) => Err(format!("daemon refused the session: {refusal}")),
        None => Ok(()),
    }
}

fn render(response: &Response) -> Result<(), String> {
    if response.status != Status::Ok {
        let error = ErrorBody::decode(&response.payload).map_err(|error| error.to_string())?;
        return Err(format!(
            "{} [{}] {}",
            response.status.as_str(),
            error.code,
            error.message
        ));
    }

    match response.api {
        Api::DiscoverDevices => {
            let mut reader = wire::Reader::new(&response.payload);
            let mut count = 0usize;
            while let Some((field, value)) = reader.next_field().map_err(|e| e.to_string())? {
                if field != 1 {
                    continue;
                }
                count += 1;
                let bytes = value.as_bytes().map_err(|e| e.to_string())?;
                let mut inner = wire::Reader::new(bytes);
                let mut id = String::new();
                let mut mode = String::new();
                let mut strength = String::new();
                while let Some((inner_field, inner_value)) =
                    inner.next_field().map_err(|e| e.to_string())?
                {
                    match inner_field {
                        1 => {
                            id = inner_value
                                .as_str(1)
                                .map_err(|e| e.to_string())?
                                .to_string()
                        }
                        3 => {
                            mode = inner_value
                                .as_str(3)
                                .map_err(|e| e.to_string())?
                                .to_string()
                        }
                        6 => {
                            strength = inner_value
                                .as_str(6)
                                .map_err(|e| e.to_string())?
                                .to_string()
                        }
                        _ => {}
                    }
                }
                println!("{id}  mode={mode}  identity={strength}");
            }
            if count == 0 {
                println!("no devices observed");
            }
        }
        Api::InspectArtifact => {
            let manifest =
                InspectArtifactResponse::decode(&response.payload).map_err(|e| e.to_string())?;
            println!("format      {}", manifest.format_id);
            println!("sha256      {}", manifest.content_sha256);
            println!("size        {} bytes", manifest.size_bytes);
            println!("confidence  {}", manifest.confidence);
            println!("members     {}", manifest.members.len());
            for member in &manifest.members {
                println!(
                    "  {:<20} {:>12}  {}  {}",
                    member.path,
                    member.size_bytes,
                    &member.sha256[..16],
                    member.role
                );
            }
            println!("partitions  {}", manifest.partitions.len());
            for partition in &manifest.partitions {
                let extent = match partition.size_sectors {
                    Some(size) => format!("{size} sectors"),
                    None => "remainder".to_string(),
                };
                println!(
                    "  {:<14} @{:<10} {}",
                    partition.name, partition.offset_sectors, extent
                );
            }
            for fact in &manifest.build_facts {
                println!("build       {} = {}", fact.key, fact.value);
            }
            for unknown in &manifest.execution_relevant_unknowns {
                println!("unknown     {} — {}", unknown.key, unknown.value);
            }
        }
        Api::MaterializePlan => {
            match MaterializePlanResponse::decode(&response.payload)
                .map_err(|error| error.to_string())?
            {
                MaterializePlanResponse::Assessment(assessment) => {
                    println!("materialization: assessment (not executable)");
                    println!("availability   : {}", assessment.availability);
                    if !assessment.unavailable_reason.is_empty() {
                        println!("reason         : {}", assessment.unavailable_reason);
                    }
                    for impact in &assessment.data_impact {
                        println!("data impact    : {} = {}", impact.key, impact.value);
                    }
                    println!(
                        "would write    : {} persistent effects",
                        assessment.known_persistent_effects.len()
                    );
                    for effect in &assessment.known_persistent_effects {
                        println!(
                            "  {:<20} {:>14} +{} bytes",
                            effect.target, effect.range_start, effect.range_length
                        );
                    }
                    for unknown in &assessment.unknowns {
                        println!("unknown        : {} — {}", unknown.key, unknown.value);
                    }
                }
                MaterializePlanResponse::Plan(plan) => {
                    println!("materialization: executable plan {}", plan.plan_id);
                    println!("plan digest    : {}", plan.plan_sha256);
                    println!("public steps   : {}", plan.public_steps.len());
                }
            }
        }
        Api::GetJob => {
            let summaries = decode_job_summaries(&response.payload)?;
            let summary = summaries
                .first()
                .ok_or("daemon returned no summary for getJob")?;
            render_job_summary(summary);
        }
        Api::ListJobs => {
            let summaries = decode_job_summaries(&response.payload)?;
            if summaries.is_empty() {
                println!("no jobs recorded");
            }
            for summary in &summaries {
                render_job_summary(summary);
            }
        }
        Api::GetRecoveryGuide => render_recovery_guide(&response.payload)?,
        other => println!("{other}: ok ({} payload bytes)", response.payload.len()),
    }
    Ok(())
}

fn decode_job_summaries(payload: &[u8]) -> Result<Vec<JobSummary>, String> {
    let mut summaries = Vec::new();
    let mut reader = wire::Reader::new(payload);
    while let Some((field, value)) = reader.next_field().map_err(|error| error.to_string())? {
        if field == 1 {
            summaries.push(
                JobSummary::decode(value.as_bytes().map_err(|error| error.to_string())?)
                    .map_err(|error| error.to_string())?,
            );
        }
    }
    Ok(summaries)
}

fn render_job_summary(summary: &JobSummary) {
    println!(
        "{}  state={}  steps={}/{}  plan={}  sequence={}",
        summary.job_id,
        summary.state,
        summary.completed_steps,
        summary.total_steps,
        summary.plan_id,
        summary.last_sequence
    );
    if !summary.current_step_id.is_empty() {
        println!("  current: {}", summary.current_step_id);
    }
    if !summary.stopped_reason.is_empty() {
        println!("  reason : {}", summary.stopped_reason);
    }
}

fn render_recovery_guide(payload: &[u8]) -> Result<(), String> {
    let mut reader = wire::Reader::new(payload);
    let mut job_id = String::new();
    let mut state = String::new();
    let mut actions = Vec::new();
    let mut supported = false;
    let mut contract = String::new();
    while let Some((field, value)) = reader.next_field().map_err(|error| error.to_string())? {
        match field {
            1 => job_id = value.as_str(1).map_err(|error| error.to_string())?.into(),
            2 => state = value.as_str(2).map_err(|error| error.to_string())?.into(),
            5 => actions.push(
                value
                    .as_str(5)
                    .map_err(|error| error.to_string())?
                    .to_string(),
            ),
            6 => supported = value.as_bool().map_err(|error| error.to_string())?,
            7 => contract = value.as_str(7).map_err(|error| error.to_string())?.into(),
            _ => {}
        }
    }
    println!("job             {job_id}");
    println!("original state  {state} (immutable)");
    println!("automatic replay forbidden");
    println!("complete overwrite supported: {supported}");
    if !contract.is_empty() {
        println!("recovery contract {contract}");
    }
    for action in actions {
        println!("  - {action}");
    }
    Ok(())
}
