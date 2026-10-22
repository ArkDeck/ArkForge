//! `completion` command.

use super::*;

pub(crate) fn run_completion(arguments: &[String], output: Output) -> Result<i32, CliError> {
    let options = Options::parse(arguments)?;
    let shell = options.one("shell")?;
    let script = completion_script(shell)?;
    match output {
        Output::Human => print!("{script}"),
        Output::Json => emit_json!(
            "{{\"schema\":\"arkforge.completion/v1\",\"ok\":true,\"shell\":{},\"script\":{}}}",
            json(shell),
            json(&script)
        ),
    }
    Ok(0)
}

fn completion_script(shell: &str) -> Result<String, CliError> {
    let mut words = BTreeSet::new();
    for spec in help::HELP {
        words.extend(spec.command.split_whitespace().map(str::to_string));
        for option in spec.typed_options() {
            words.insert(format!("--{}", option.name));
        }
    }
    let words = words.into_iter().collect::<Vec<_>>().join(" ");
    match shell {
        "bash" => Ok(format!(
            "_arkforge_complete() {{\n  local cur=\"${{COMP_WORDS[COMP_CWORD]}}\"\n  COMPREPLY=( $(compgen -W '{}' -- \"$cur\") )\n}}\ncomplete -F _arkforge_complete arkforge\n",
            words
        )),
        "zsh" => Ok(format!(
            "#compdef arkforge\n_arkforge() {{\n  local -a words\n  words=({})\n  _describe 'arkforge command or option' words\n}}\ncompdef _arkforge arkforge\n",
            words
        )),
        "fish" => Ok(format!("complete -c arkforge -f -a '{}'\n", words)),
        value => Err(CliError::invalid(format!(
            "--shell accepts 'bash', 'zsh', or 'fish', not {value:?}."
        ))),
    }
}
