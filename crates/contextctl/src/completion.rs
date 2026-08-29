use clap::ValueEnum;
use serde::Serialize;

const TOP_LEVEL: &str = "init paths ping stats compose search commit-work retry-spool pack entry review export import source-import policy run setup doctor completion hook";

#[derive(Clone, Copy, Debug, Serialize, ValueEnum)]
#[serde(rename_all = "lowercase")]
pub(crate) enum CompletionShell {
    Bash,
    Zsh,
    Fish,
}

#[derive(Clone, Debug, Serialize)]
pub(crate) struct CompletionOutput {
    pub shell: CompletionShell,
    pub script: String,
}

pub(crate) fn generate(shell: CompletionShell) -> CompletionOutput {
    let script = match shell {
        CompletionShell::Bash => bash_completion(),
        CompletionShell::Zsh => zsh_completion(),
        CompletionShell::Fish => fish_completion(),
    };
    CompletionOutput { shell, script }
}

fn bash_completion() -> String {
    format!(
        r#"_contextctl_complete() {{
  local cur="${{COMP_WORDS[COMP_CWORD]}}"
  local words="{TOP_LEVEL} --json --db --socket --help --version"
  case "${{COMP_WORDS[1]}}" in
    pack) words="create list update --help" ;;
    entry) words="put get list delete revert --help" ;;
    review) words="list approve reject edit --help" ;;
    run) words="create list --help" ;;
    policy) words="show set --help" ;;
    source-import) words="preview apply --help" ;;
    hook) words="compose commit retry-spool --help" ;;
  esac
  COMPREPLY=( $(compgen -W "$words" -- "$cur") )
}}
complete -F _contextctl_complete contextctl
"#
    )
}

fn zsh_completion() -> String {
    format!(
        r#"#compdef contextctl
_contextctl() {{
  local parent="${{words[2]}}"
  local -a candidates
  candidates=({TOP_LEVEL} --json --db --socket --help --version)
  case "$parent" in
    pack) candidates=(create list update --help) ;;
    entry) candidates=(put get list delete revert --help) ;;
    review) candidates=(list approve reject edit --help) ;;
    run) candidates=(create list --help) ;;
    policy) candidates=(show set --help) ;;
    source-import) candidates=(preview apply --help) ;;
    hook) candidates=(compose commit retry-spool --help) ;;
  esac
  _describe 'contextctl command' candidates
}}
compdef _contextctl contextctl
"#
    )
}

fn fish_completion() -> String {
    let mut script = String::from("complete -c contextctl -f\n");
    for command in TOP_LEVEL.split_whitespace() {
        script.push_str(&format!(
            "complete -c contextctl -n '__fish_use_subcommand' -a '{command}'\n"
        ));
    }
    for (parent, children) in [
        ("pack", "create list update"),
        ("entry", "put get list delete revert"),
        ("review", "list approve reject edit"),
        ("run", "create list"),
        ("policy", "show set"),
        ("source-import", "preview apply"),
        ("hook", "compose commit retry-spool"),
    ] {
        for child in children.split_whitespace() {
            script.push_str(&format!(
                "complete -c contextctl -n '__fish_seen_subcommand_from {parent}' -a '{child}'\n"
            ));
        }
    }
    script
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn completion_scripts_cover_new_command_groups() {
        for shell in [
            CompletionShell::Bash,
            CompletionShell::Zsh,
            CompletionShell::Fish,
        ] {
            let output = generate(shell);
            assert!(output.script.contains("source-import"));
            assert!(output.script.contains("doctor"));
            assert!(output.script.contains("policy"));
        }
    }
}
