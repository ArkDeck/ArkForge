//! The final whole-tree consistency sweep (WF-AC-27).
//!
//! Every surface this build publishes — the parser, human help, the JSON help
//! index, and shell completion — is generated from one typed command tree. This
//! file is the check that they still agree after the whole redesign, and that
//! no command removed along the way survives anywhere.

use std::collections::BTreeSet;
use std::process::Command;

fn run(arguments: &[&str]) -> String {
    let output = Command::new(env!("CARGO_BIN_EXE_arkforge"))
        .args(arguments)
        .output()
        .expect("canonical arkforge CLI should start");
    assert!(output.status.success(), "{arguments:?} -> {output:?}");
    String::from_utf8(output.stdout).unwrap()
}

/// Every `"key":` name at the top level of one rendered leaf.
///
/// The leaf ends where its own brace closes, so a following leaf in the same
/// document is never read as part of this one.
fn leaf_members(leaf: &str) -> BTreeSet<String> {
    let bytes = leaf.as_bytes();
    let mut members = BTreeSet::new();
    let mut depth = 0usize;
    let mut index = 0usize;
    while index < bytes.len() {
        match bytes[index] {
            b'{' | b'[' => {
                depth += 1;
                index += 1;
            }
            b'}' | b']' => {
                depth -= 1;
                if depth == 0 {
                    break;
                }
                index += 1;
            }
            b'"' => {
                let start = index + 1;
                let mut end = start;
                while end < bytes.len() {
                    if bytes[end] == b'\\' {
                        end += 2;
                        continue;
                    }
                    if bytes[end] == b'"' {
                        break;
                    }
                    end += 1;
                }
                if depth == 1 && bytes.get(end + 1) == Some(&b':') {
                    members.insert(leaf[start..end].to_string());
                }
                index = end + 1;
            }
            _ => index += 1,
        }
    }
    members
}

fn commands(index: &str) -> Vec<String> {
    index
        .split("\"command\":\"")
        .skip(1)
        .map(|rest| rest.split_once('"').unwrap().0.to_string())
        .collect()
}

#[test]
fn every_published_surface_describes_the_same_command_tree() {
    let index = run(&["help", "--all", "--format", "json"]);
    let commands = commands(&index);
    assert!(
        commands.len() > 20,
        "the tree looks truncated: {commands:?}"
    );

    // The parser and human help know every command the index lists.
    for command in &commands {
        if command.is_empty() {
            continue;
        }
        let mut arguments = vec!["help"];
        arguments.extend(command.split_whitespace());
        let human = run(&arguments);
        assert!(human.contains("Usage:"), "{command} has no usage");
        assert!(human.contains("Effect:"), "{command} has no effect");
        assert!(human.contains("Exit codes:"), "{command} has no exit codes");
    }

    // Completion offers exactly the words the tree defines, and nothing else.
    let script = run(&["completion", "--shell", "bash"]);
    let offered = script
        .split_once("compgen -W '")
        .unwrap()
        .1
        .split_once('\'')
        .unwrap()
        .0
        .split_whitespace()
        .map(str::to_string)
        .collect::<BTreeSet<_>>();
    let mut expected = BTreeSet::new();
    for command in &commands {
        expected.extend(command.split_whitespace().map(str::to_string));
    }
    for option in index.split("\"name\":\"--").skip(1) {
        expected.insert(format!("--{}", option.split_once('"').unwrap().0));
    }
    assert_eq!(
        offered, expected,
        "completion drifted from the command tree"
    );
}

#[test]
fn every_leaf_carries_the_v1_contract_plus_exactly_the_two_additive_members() {
    let index = run(&["help", "--all", "--format", "json"]);
    // The members CHG-2026-CLI-arkforge-agent-native-cli published.
    let v1: BTreeSet<String> = [
        "schema",
        "path",
        "command",
        "summary",
        "usage",
        "effect",
        "effect_detail",
        "interactive",
        "availability",
        "subcommands",
        "requires",
        "outputs",
        "output_descriptions",
        "options",
        "constraints",
        "examples",
        "next_commands",
        "exit_codes",
    ]
    .iter()
    .map(|name| name.to_string())
    .collect();
    // This change adds these two and removes nothing.
    let added: BTreeSet<String> = ["runtime_effect", "facts_projections"]
        .iter()
        .map(|name| name.to_string())
        .collect();

    let mut leaves = 0;
    for leaf in index
        .split("{\"schema\":\"arkforge.command-help/v1\",")
        .skip(1)
    {
        let leaf = format!("{{\"schema\":\"arkforge.command-help/v1\",{leaf}");
        let members = leaf_members(&leaf);
        assert!(
            v1.is_subset(&members),
            "a leaf dropped v1 members: {:?}",
            v1.difference(&members).collect::<Vec<_>>()
        );
        let extra = members.difference(&v1).cloned().collect::<BTreeSet<_>>();
        assert_eq!(extra, added, "a leaf changed the additive member set");
        leaves += 1;
    }
    assert!(leaves > 20, "only {leaves} leaves were inspected");
}

#[test]
fn no_command_this_change_removed_survives_anywhere() {
    let removed = [
        "doctor",
        "daemon status",
        "device show",
        "device probe",
        "artifact inspect",
        "flash assess",
        "flash apply",
        "job watch",
        "job cancel",
        "job recovery",
        "job recovery guide",
        "job recovery plan",
    ];
    let index = run(&["help", "--all", "--format", "json"]);
    for command in removed {
        assert!(
            !index.contains(&format!("\"command\":\"{command}\"")),
            "{command} is still a leaf"
        );
        assert!(
            !index.contains(&format!("arkforge {command} ")),
            "{command} is still referenced by another leaf"
        );
        let invoked = Command::new(env!("CARGO_BIN_EXE_arkforge"))
            .args(command.split_whitespace())
            .output()
            .unwrap();
        assert_eq!(
            invoked.status.code(),
            Some(2),
            "{command} is still accepted by the parser"
        );
    }
    // The schemas those commands owned are gone with them.
    for schema in [
        "arkforge.doctor/v1",
        "arkforge.device-probe/v1",
        "arkforge.device-observation/v1",
        "arkforge.recovery-guide/v1",
        "arkforge.flash-assessment/v1",
        "arkforge.flash-plan/v1",
        "arkforge.artifact/v1",
    ] {
        assert!(!index.contains(schema), "{schema} is still published");
    }
}

#[test]
fn the_surfaces_this_change_added_are_all_present() {
    let index = run(&["help", "--all", "--format", "json"]);
    for command in [
        "status",
        "apply",
        "watch",
        "cancel",
        "flash run",
        "flash plan",
        "device list",
        "artifact show",
        "job show",
        "job recover",
        "config show",
        "config set",
        "config unset",
        "config add",
        "config remove",
    ] {
        assert!(
            index.contains(&format!("\"command\":\"{command}\"")),
            "{command} is missing from the tree"
        );
    }
    for schema in [
        "arkforge.status/v1",
        "arkforge.command-help-index/v1",
        "arkforge.flash-plan/v2",
        "arkforge.config/v1",
        "arkforge.cli-approval/v1",
    ] {
        assert!(index.contains(schema), "{schema} is not published anywhere");
    }
}
