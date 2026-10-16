//! `config` command.

use super::*;

/// Reusable local bindings: what this host may drive, not what it may do.
pub(crate) fn run_config(arguments: &[String], globals: Globals) -> Result<i32, CliError> {
    let Some(subcommand) = arguments.first() else {
        help::print_help(help::help_spec(&["config".into()])?, globals.output);
        return Ok(0);
    };
    let runtime_dir = command_runtime_dir(&globals)?;
    let settings = Settings::parse(&arguments[1..])?;
    let mut config = RuntimeConfig::load(&runtime_dir)?;
    match subcommand.as_str() {
        "show" => {
            settings.reject_any("config show")?;
            print_config(globals.output, &config, &runtime_dir);
            return Ok(0);
        }
        "set" => {
            // A path and the digest of its bytes are one decision, so they are
            // one transaction: a config can never name a tool it has not pinned.
            match (settings.value("hdc.path"), settings.value("hdc.sha256")) {
                (Some(path), Some(expected)) => {
                    let pinned = pin(Path::new(path))?;
                    if pinned.sha256 != expected {
                        return Err(CliError::new(
                            "CONFIG_DIGEST_MISMATCH",
                            "The file at the configured path does not have the expected digest; nothing was stored."
                                .to_string(),
                            3,
                            false,
                        ));
                    }
                    config.hdc = Some(pinned);
                }
                (None, None) => {}
                _ => {
                    return Err(CliError::invalid(
                        "hdc.path and hdc.sha256 bind one tool together; supply both or neither.",
                    ));
                }
            }
            if let Some(value) = settings.value("daemon.require-release-signing") {
                config.require_release_signing = value == "true";
            }
            settings.require_any()?;
        }
        "unset" => {
            for key in settings.keys() {
                match key.as_str() {
                    "hdc" => config.hdc = None,
                    "daemon.require-release-signing" => config.require_release_signing = false,
                    other => {
                        return Err(CliError::invalid(format!(
                            "Unknown setting {other:?} to clear."
                        )));
                    }
                }
            }
            settings.require_any()?;
        }
        "add" => {
            let (Some(path), Some(expected)) = (
                settings.value("profile-file.path"),
                settings.value("profile-file.sha256"),
            ) else {
                return Err(CliError::invalid(
                    "profile-file.path and profile-file.sha256 bind one profile together; supply both.",
                ));
            };
            let pinned = pin(Path::new(path))?;
            if pinned.sha256 != expected {
                return Err(CliError::new(
                    "CONFIG_DIGEST_MISMATCH",
                    "The file at the configured path does not have the expected digest; nothing was stored.".to_string(),
                    3,
                    false,
                ));
            }
            if config
                .profile_files
                .iter()
                .any(|existing| existing.sha256 == pinned.sha256)
            {
                return Err(CliError::new(
                    "CONFIG_ALREADY_BOUND",
                    format!(
                        "A profile with digest {} is already configured.",
                        pinned.sha256
                    ),
                    6,
                    false,
                ));
            }
            config.profile_files.push(pinned);
            config
                .profile_files
                .sort_by(|left, right| left.sha256.cmp(&right.sha256));
        }
        "remove" => {
            let Some(expected) = settings.value("profile-file.sha256") else {
                return Err(CliError::invalid(
                    "Name the profile to remove by digest: profile-file.sha256=<sha256>.",
                ));
            };
            let before = config.profile_files.len();
            config
                .profile_files
                .retain(|profile| profile.sha256 != expected);
            if config.profile_files.len() == before {
                return Err(CliError::new(
                    "CONFIG_NOT_BOUND",
                    format!("No configured profile has digest {expected}."),
                    5,
                    false,
                ));
            }
        }
        other => {
            return Err(CliError::invalid(format!(
                "Unknown config command {other:?}. Run 'arkforge help config'."
            )));
        }
    }
    config.store(&runtime_dir)?;
    print_config(globals.output, &config, &runtime_dir);
    Ok(0)
}

/// The `key=value` operands one config command was given.
struct Settings {
    values: Vec<(String, Option<String>)>,
}

impl Settings {
    fn parse(arguments: &[String]) -> Result<Self, CliError> {
        let mut values = Vec::new();
        for argument in arguments {
            let (key, value) = match argument.split_once('=') {
                Some((key, value)) => (key.to_string(), Some(value.to_string())),
                None => (argument.clone(), None),
            };
            if values.iter().any(|(existing, _)| *existing == key) {
                return Err(CliError::invalid(format!(
                    "{key} may be supplied only once."
                )));
            }
            values.push((key, value));
        }
        Ok(Self { values })
    }

    fn value(&self, key: &str) -> Option<&str> {
        self.values
            .iter()
            .find(|(name, _)| name == key)
            .and_then(|(_, value)| value.as_deref())
    }

    fn keys(&self) -> Vec<String> {
        self.values.iter().map(|(key, _)| key.clone()).collect()
    }

    fn require_any(&self) -> Result<(), CliError> {
        if self.values.is_empty() {
            return Err(CliError::invalid(
                "Name at least one setting; run 'arkforge help config --format json' for the exact keys.",
            ));
        }
        Ok(())
    }

    fn reject_any(&self, command: &str) -> Result<(), CliError> {
        if self.values.is_empty() {
            return Ok(());
        }
        Err(CliError::invalid(format!(
            "{command} accepts no settings; unexpected {:?}.",
            self.values[0].0
        )))
    }
}

/// Reports what is bound without reporting where it lives.
///
/// Structured output carries binding state, digests and counts only. A host
/// path is not a secret to the owner reading their own terminal, but it is not
/// something an Agent transcript or a CI log should carry, and CLI-AC-04 draws
/// that line at the structured surface.
fn print_config(output: Output, config: &RuntimeConfig, runtime_dir: &Path) {
    let next = if config.hdc.is_some() {
        "arkforge status".to_string()
    } else {
        "arkforge config set hdc.path=<absolute-path> hdc.sha256=<sha256>".to_string()
    };
    match output {
        Output::Human => {
            println!("ArkForge configuration ({})", runtime_dir.display());
            match &config.hdc {
                Some(hdc) => {
                    println!("hdc");
                    println!("  path    {}", hdc.path.display());
                    println!("  sha256  {}", hdc.sha256);
                }
                None => println!("hdc      not bound"),
            }
            println!("profile files ({})", config.profile_files.len());
            for profile in &config.profile_files {
                println!("  {}  {}", profile.sha256, profile.path.display());
            }
            println!(
                "daemon.require-release-signing  {}",
                config.require_release_signing
            );
            println!("Campaigns are never stored; name one per call with --hardware-campaign.");
            println!("Next: {next}");
        }
        Output::Json => emit_json!(
            "{{\"schema\":\"arkforge.config/v1\",\"hdc\":{},\"profile_files\":[{}],\"profile_file_count\":{},\"daemon\":{{\"require_release_signing\":{}}},\"campaign_persistable\":false,\"next_commands\":[{}]}}",
            config
                .hdc
                .as_ref()
                .map(|hdc| format!("{{\"bound\":true,\"sha256\":{}}}", json(&hdc.sha256)))
                .unwrap_or_else(|| "{\"bound\":false,\"sha256\":null}".to_string()),
            config
                .profile_files
                .iter()
                .map(|profile| format!("{{\"sha256\":{}}}", json(&profile.sha256)))
                .collect::<Vec<_>>()
                .join(","),
            config.profile_files.len(),
            config.require_release_signing,
            json(&next)
        ),
    }
}
