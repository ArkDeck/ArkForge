//! `signing` command.

use super::*;

pub(crate) fn run_signing(arguments: &[String], output: Output) -> Result<i32, CliError> {
    let Some(subcommand) = arguments.first() else {
        help::print_help(help::help_spec(&["signing".into()])?, output);
        return Ok(0);
    };
    if subcommand != "verify" {
        return Err(CliError::invalid(format!(
            "Unknown signing command {subcommand:?}. Run 'arkforge help signing'."
        )));
    }

    let options = Options::parse(&arguments[1..])?;
    let file = Path::new(options.one("file")?);
    let (mode, mode_name) = match options.one("mode")? {
        "development" => (ContractMode::Development, "development"),
        "release" => (ContractMode::Release, "release"),
        value => {
            return Err(CliError::invalid(format!(
                "--mode accepts 'development' or 'release', not {value:?}."
            )));
        }
    };
    let input = std::fs::read(file).map_err(|error| {
        CliError::new(
            "SIGNING_INPUT_REFUSED",
            format!("Unable to read the selected signing input: {error}"),
            3,
            false,
        )
    })?;
    let input_digest = arkforge_core::digest::sha256(&input);
    let code = packaging::read(&input).map_err(|error| {
        CliError::new(
            "SIGNING_INPUT_REFUSED",
            format!("Unable to inspect the selected signing input: {error}"),
            3,
            false,
        )
    })?;
    let violations = code.violations(mode);
    print_signing(output, file, input_digest, mode_name, &code, &violations);
    Ok(if violations.is_empty() { 0 } else { 3 })
}
fn print_signing(
    output: Output,
    file: &Path,
    input_digest: Sha256Digest,
    mode: &str,
    code: &SignedCode,
    violations: &[packaging::ContractViolation],
) {
    let compliant = violations.is_empty();
    match output {
        Output::Human => {
            println!(
                "Signing contract: {}",
                if compliant { "compliant" } else { "refused" }
            );
            println!("file     {}", file.display());
            println!("sha256   {input_digest}");
            println!("mode     {mode}");
            println!("facts    {}", code.summary());
            if compliant {
                println!("No further signing action is required.");
            } else {
                println!("violations");
                for violation in violations {
                    println!("  {}: {violation}", violation.code());
                }
                println!(
                    "Next: Correct every listed signing fact, then run this exact verification again."
                );
            }
        }
        Output::Json => {
            let violations_json = violations
                .iter()
                .map(|violation| {
                    format!(
                        "{{\"code\":{},\"message\":{}}}",
                        json(violation.code()),
                        json(&violation.to_string())
                    )
                })
                .collect::<Vec<_>>()
                .join(",");
            let next = if compliant {
                Vec::new()
            } else {
                vec!["arkforge help signing verify --format json".to_string()]
            };
            emit_json!(
                "{{\"schema\":\"arkforge.signing-verification/v1\",\"input_sha256\":{},\"mode\":{},\"compliant\":{},\"facts\":{},\"violations\":[{}],\"contract\":{},\"next_commands\":{}}}",
                json(&input_digest.to_string()),
                json(mode),
                compliant,
                json(&code.summary()),
                violations_json,
                json(packaging::CONTRACT_DOC),
                json_array(&next)
            );
        }
    }
}
