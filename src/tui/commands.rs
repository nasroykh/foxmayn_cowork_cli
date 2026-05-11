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
