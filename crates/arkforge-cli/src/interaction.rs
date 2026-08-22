//! When this frontend may ask a person something, and how it asks.
//!
//! The rule is deliberately narrow (design.md 1.1). A command may ask only when
//! all three standard streams are terminals, the output format is the human one,
//! and the caller did not opt out. Any redirection at all closes the gate: a
//! command whose stdout is a pipe is being read by a program, and a program
//! cannot answer a question — it can only hang while one is asked.
//!
//! Everything that decides *what* to ask lives behind [`Prompt`], so the rules
//! can be exercised against a scripted operator instead of a terminal.

use std::io::{BufRead, IsTerminal, Write};

/// Whether a call may ask its operator anything.
///
/// Split from the terminal probing so the rule itself is testable: this is the
/// whole of it, and every argument is a fact about the invocation.
pub fn gate_open(
    human_output: bool,
    no_input: bool,
    stdin: bool,
    stdout: bool,
    stderr: bool,
) -> bool {
    human_output && !no_input && stdin && stdout && stderr
}

/// The gate for this process.
pub fn open_for(human_output: bool, no_input: bool) -> bool {
    gate_open(
        human_output,
        no_input,
        std::io::stdin().is_terminal(),
        std::io::stdout().is_terminal(),
        std::io::stderr().is_terminal(),
    )
}

/// Somebody who can be shown a line and asked a question.
pub trait Prompt {
    fn show(&mut self, line: &str);
    /// Asks and returns the trimmed answer, or `None` if the operator ended the
    /// input instead of answering.
    fn ask(&mut self, question: &str) -> Option<String>;
}

/// The real operator, on the terminal this process is attached to.
pub struct TerminalPrompt;

impl Prompt for TerminalPrompt {
    fn show(&mut self, line: &str) {
        println!("{line}");
    }

    fn ask(&mut self, question: &str) -> Option<String> {
        print!("{question}");
        let _ = std::io::stdout().flush();
        let mut answer = String::new();
        match std::io::stdin().lock().read_line(&mut answer) {
            Ok(0) | Err(_) => None,
            Ok(_) => Some(answer.trim().to_string()),
        }
    }
}

/// One numbered choice offered to the operator.
pub struct Choice {
    /// The value the caller gets back when this line is chosen.
    pub value: String,
    /// What the operator reads.
    pub label: String,
}

/// Asks the operator to pick one of several candidates.
///
/// A numbered line list and nothing more: no raw mode, no full-screen frame, no
/// filesystem browser. An empty answer or an ended input selects nothing, so
/// pressing return out of habit cannot pick a device.
pub fn select(prompt: &mut dyn Prompt, title: &str, choices: &[Choice]) -> Option<String> {
    if choices.is_empty() {
        return None;
    }
    prompt.show(title);
    for (index, choice) in choices.iter().enumerate() {
        prompt.show(&format!("  {}) {}", index + 1, choice.label));
    }
    let answer = prompt.ask(&format!("Select [1-{}]: ", choices.len()))?;
    let index = answer.parse::<usize>().ok()?;
    if index == 0 || index > choices.len() {
        return None;
    }
    Some(choices[index - 1].value.clone())
}

/// What the confirmation screen must hear before it accepts.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Confirmation {
    /// A plain `y`, for a board this host has proved and flashed before.
    Acknowledge,
    /// One of the product models the profile declares, typed in full.
    TypeModel(Vec<String>),
}

impl Confirmation {
    /// Which confirmation an operator owes for this device.
    ///
    /// Typing a model name is not a formality. It is required whenever the
    /// machine cannot prove which board this is — every time, since a human
    /// assertion never becomes evidence — and once more for the first flash of
    /// a board and profile this host has proved but not yet written to.
    pub fn required(
        identity_is_strong: bool,
        first_flash: bool,
        declared_models: &[String],
    ) -> Self {
        if identity_is_strong && !first_flash {
            return Confirmation::Acknowledge;
        }
        Confirmation::TypeModel(declared_models.to_vec())
    }

    /// Whether an answer satisfies this confirmation.
    pub fn accepts(&self, answer: &str) -> bool {
        match self {
            Confirmation::Acknowledge => answer.eq_ignore_ascii_case("y"),
            Confirmation::TypeModel(models) => models
                .iter()
                .any(|model| model.eq_ignore_ascii_case(answer)),
        }
    }

    pub fn question(&self) -> String {
        match self {
            Confirmation::Acknowledge => "Type y to accept these effects: ".to_string(),
            Confirmation::TypeModel(models) => format!(
                "Type the product model to accept these effects ({}): ",
                models.join(" or ")
            ),
        }
    }
}

#[cfg(test)]
pub struct ScriptedPrompt {
    pub answers: Vec<String>,
    pub shown: Vec<String>,
    pub asked: Vec<String>,
}

#[cfg(test)]
impl ScriptedPrompt {
    pub fn new(answers: &[&str]) -> Self {
        Self {
            answers: answers
                .iter()
                .rev()
                .map(|value| value.to_string())
                .collect(),
            shown: Vec::new(),
            asked: Vec::new(),
        }
    }
}

#[cfg(test)]
impl Prompt for ScriptedPrompt {
    fn show(&mut self, line: &str) {
        self.shown.push(line.to_string());
    }

    fn ask(&mut self, question: &str) -> Option<String> {
        self.asked.push(question.to_string());
        self.answers.pop()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn any_redirected_stream_closes_the_gate() {
        assert!(gate_open(true, false, true, true, true));
        // A program is reading one of these, and a program cannot answer.
        assert!(!gate_open(true, false, false, true, true));
        assert!(!gate_open(true, false, true, false, true));
        assert!(!gate_open(true, false, true, true, false));
        // Structured output and an explicit opt-out close it too.
        assert!(!gate_open(false, false, true, true, true));
        assert!(!gate_open(true, true, true, true, true));
    }

    #[test]
    fn a_selector_takes_only_an_exact_line_number() {
        let choices = [
            Choice {
                value: "one".into(),
                label: "first".into(),
            },
            Choice {
                value: "two".into(),
                label: "second".into(),
            },
        ];
        let mut prompt = ScriptedPrompt::new(&["2"]);
        assert_eq!(
            select(&mut prompt, "Select a device", &choices).as_deref(),
            Some("two")
        );
        assert!(prompt.shown.iter().any(|line| line.contains("1) first")));

        // Habitually pressing return must not choose a device.
        for answer in ["", "0", "3", "yes"] {
            let mut prompt = ScriptedPrompt::new(&[answer]);
            assert_eq!(select(&mut prompt, "Select a device", &choices), None);
        }
    }

    #[test]
    fn a_model_name_is_owed_whenever_the_board_is_unproven() {
        let models = vec!["DAYU200".to_string()];
        // Unproven identity: every time, no matter how often it was flashed.
        assert_eq!(
            Confirmation::required(false, false, &models),
            Confirmation::TypeModel(models.clone())
        );
        assert_eq!(
            Confirmation::required(false, true, &models),
            Confirmation::TypeModel(models.clone())
        );
        // Proven identity: once for the first flash, then a plain acceptance.
        assert_eq!(
            Confirmation::required(true, true, &models),
            Confirmation::TypeModel(models.clone())
        );
        assert_eq!(
            Confirmation::required(true, false, &models),
            Confirmation::Acknowledge
        );
    }

    #[test]
    fn a_confirmation_accepts_only_what_it_asked_for() {
        let acknowledge = Confirmation::Acknowledge;
        assert!(acknowledge.accepts("y"));
        assert!(acknowledge.accepts("Y"));
        assert!(!acknowledge.accepts(""));
        assert!(!acknowledge.accepts("yes"));

        let model = Confirmation::TypeModel(vec!["DAYU200".into()]);
        assert!(model.accepts("dayu200"));
        assert!(!model.accepts("y"));
        assert!(!model.accepts("DAYU"));
        assert!(model.question().contains("DAYU200"));
    }
}
