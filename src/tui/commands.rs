pub struct SlashCommand {
    pub name: &'static str,
    pub description: &'static str,
    /// Whether the command takes a trailing argument (e.g. "/dir <path>")
    pub has_arg: bool,
    /// Whether entering the command with no argument opens an interactive picker.
    pub has_picker: bool,
    /// Fixed option list shown in the picker. Empty for commands with dynamic items (e.g. /resume).
    pub static_options: &'static [&'static str],
}

pub static COMMANDS: &[SlashCommand] = &[
    SlashCommand {
        name: "/clear",
        description: "Clear the conversation",
        has_arg: false,
        has_picker: false,
        static_options: &[],
    },
    SlashCommand {
        name: "/exit",
        description: "Quit the application",
        has_arg: false,
        has_picker: false,
        static_options: &[],
    },
    SlashCommand {
        name: "/dir",
        description: "Change working directory",
        has_arg: true,
        has_picker: false,
        static_options: &[],
    },
    SlashCommand {
        name: "/model",
        description: "Browse or switch model",
        has_arg: true,
        has_picker: true,
        static_options: &[],
    },
    SlashCommand {
        name: "/skip-confirmations",
        description: "Toggle confirmation prompts",
        has_arg: false,
        has_picker: false,
        static_options: &[],
    },
    SlashCommand {
        name: "/streaming",
        description: "Toggle token-by-token streaming",
        has_arg: false,
        has_picker: false,
        static_options: &[],
    },
    SlashCommand {
        name: "/thinking",
        description: "Set thinking display",
        has_arg: true,
        has_picker: true,
        static_options: &["off", "inline", "full"],
    },
    SlashCommand {
        name: "/tool-verbosity",
        description: "Set tool display verbosity",
        has_arg: true,
        has_picker: true,
        static_options: &["default", "minimal", "full"],
    },
    SlashCommand {
        name: "/reasoning",
        description: "Set reasoning/thinking level for the LLM",
        has_arg: true,
        has_picker: true,
        static_options: &[],
    },
    SlashCommand {
        name: "/sessions",
        description: "List past sessions for this project",
        has_arg: false,
        has_picker: false,
        static_options: &[],
    },
    SlashCommand {
        name: "/resume",
        description: "Resume a past session (browse or /resume <id>)",
        has_arg: true,
        has_picker: true,
        static_options: &[],
    },
];

/// Returns indices into COMMANDS whose name starts with `prefix`.
pub fn match_commands(prefix: &str) -> Vec<usize> {
    COMMANDS
        .iter()
        .enumerate()
        .filter(|(_, cmd)| cmd.name.starts_with(prefix))
        .map(|(i, _)| i)
        .collect()
}

/// What Enter should do while the slash-completions popup is open.
#[derive(Debug, PartialEq, Eq)]
pub enum SlashEnter {
    /// Submit the typed text as-is (the command already carries an argument, or is typed in full).
    Submit,
    /// Run the highlighted command with no argument (opens its picker, or executes it).
    Run(usize),
    /// Complete the highlighted command into the input (plus a space) so the user can type its arg.
    Complete(usize),
}

/// Decide what Enter does given the raw input and the highlighted command index.
///
/// Typed text that already has an argument (any whitespace after the command word) or that exactly
/// names a command is submitted untouched; only a partial command word is completed.
pub fn decide_enter(input: &str, highlighted: usize) -> SlashEnter {
    let typed = input.trim();
    if typed.contains(char::is_whitespace) {
        return SlashEnter::Submit;
    }
    let Some(cmd) = COMMANDS.get(highlighted) else {
        return SlashEnter::Submit;
    };
    if typed == cmd.name {
        // A bare arg-taking command with no picker (e.g. `/dir`) has nothing to run yet.
        return if cmd.has_arg && !cmd.has_picker {
            SlashEnter::Complete(highlighted)
        } else {
            SlashEnter::Submit
        };
    }
    if cmd.has_picker || !cmd.has_arg {
        SlashEnter::Run(highlighted)
    } else {
        SlashEnter::Complete(highlighted)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn idx(name: &str) -> usize {
        COMMANDS.iter().position(|c| c.name == name).unwrap()
    }

    #[test]
    fn enter_with_arg_submits() {
        for text in ["/dir /tmp", "/model x", "/resume 3", "/thinking off"] {
            let first = text.split_whitespace().next().unwrap();
            let i = match_commands(first)[0];
            assert_eq!(decide_enter(text, i), SlashEnter::Submit, "{text}");
        }
    }

    #[test]
    fn enter_exact_name_submits() {
        assert_eq!(decide_enter("/model", idx("/model")), SlashEnter::Submit);
        assert_eq!(decide_enter("/clear", idx("/clear")), SlashEnter::Submit);
    }

    #[test]
    fn enter_bare_arg_command_completes() {
        assert_eq!(
            decide_enter("/dir", idx("/dir")),
            SlashEnter::Complete(idx("/dir"))
        );
    }

    #[test]
    fn enter_partial_word_completes_or_runs() {
        assert_eq!(
            decide_enter("/di", idx("/dir")),
            SlashEnter::Complete(idx("/dir"))
        );
        assert_eq!(
            decide_enter("/mo", idx("/model")),
            SlashEnter::Run(idx("/model"))
        );
        assert_eq!(
            decide_enter("/cl", idx("/clear")),
            SlashEnter::Run(idx("/clear"))
        );
    }
}
