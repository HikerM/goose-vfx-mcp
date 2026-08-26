use anyhow::{anyhow, Result};
use chrono;
use lumina::config::Config;
use lumina::conversation::message::{Message, MessageContent, MessageMetadata};
use lumina::session::{SessionManager, SessionType};
use rmcp::model::Role;

use crate::session::{build_session, SessionBuilderConfig};

use clap::ValueEnum;

#[derive(ValueEnum, Clone, Copy, Debug, PartialEq, Eq)]
pub enum Shell {
    Bash,
    Zsh,
    Fish,
    #[value(alias = "nushell")]
    Nu,
    #[value(alias = "pwsh")]
    Powershell,
}

struct ShellConfig {
    script_template: &'static str,
    command_not_found: Option<&'static str>,
}

impl Shell {
    fn config(&self) -> &'static ShellConfig {
        match self {
            Shell::Bash => &BASH_CONFIG,
            Shell::Zsh => &ZSH_CONFIG,
            Shell::Fish => &FISH_CONFIG,
            Shell::Nu => &NU_CONFIG,
            Shell::Powershell => &POWERSHELL_CONFIG,
        }
    }
}

static BASH_CONFIG: ShellConfig = ShellConfig {
    script_template: r#"export AGENT_SESSION_ID="{session_id}"
alias @lumina='{lumina_bin} term run'
alias @g='{lumina_bin} term run'

lumina_preexec() {
    [[ "$1" =~ ^lumina\ term ]] && return
    [[ "$1" =~ ^(@lumina|@g)($|[[:space:]]) ]] && return
    ('{lumina_bin}' term log "$1" &) 2>/dev/null
}

if [[ -z "$lumina_preexec_installed" ]]; then
    lumina_preexec_installed=1
    trap 'lumina_preexec "$BASH_COMMAND"' DEBUG
fi{command_not_found_handler}"#,
    command_not_found: Some(
        r#"

command_not_found_handle() {
    echo "🪿 Command '$1' not found. Asking lumina..."
    '{lumina_bin}' term run "$@"
    return 0
}"#,
    ),
};

static ZSH_CONFIG: ShellConfig = ShellConfig {
    script_template: r#"export AGENT_SESSION_ID="{session_id}"
alias @lumina='{lumina_bin} term run'
alias @g='{lumina_bin} term run'

lumina_preexec() {
    [[ "$1" =~ ^lumina\ term ]] && return
    [[ "$1" =~ ^(@lumina|@g)($|[[:space:]]) ]] && return
    ('{lumina_bin}' term log "$1" &) 2>/dev/null
}

autoload -Uz add-zsh-hook
add-zsh-hook preexec lumina_preexec{command_not_found_handler}"#,
    command_not_found: Some(
        r#"

command_not_found_handler() {
    echo "🪿 Command '$1' not found. Asking lumina..."
    '{lumina_bin}' term run "$@"
    return 0
}"#,
    ),
};

static FISH_CONFIG: ShellConfig = ShellConfig {
    script_template: r#"set -gx AGENT_SESSION_ID "{session_id}"
function @lumina; {lumina_bin} term run $argv; end
function @g; {lumina_bin} term run $argv; end

function lumina_preexec --on-event fish_preexec
    string match -q -r '^lumina term' -- $argv[1]; and return
    string match -q -r '^(@lumina|@g)($|\s)' -- $argv[1]; and return
    {lumina_bin} term log "$argv[1]" 2>/dev/null &
end"#,
    command_not_found: None,
};

static NU_CONFIG: ShellConfig = ShellConfig {
    script_template: r#"$env.AGENT_SESSION_ID = "{session_id}"
def --wrapped @lumina [...args] { run-external "{lumina_bin}" "term" "run" ...$args }
def --wrapped @g [...args] { run-external "{lumina_bin}" "term" "run" ...$args }

if (($env | get -o LUMINA_NU_PREEXEC_INSTALLED | default false) != true) {
    $env.LUMINA_NU_PREEXEC_INSTALLED = true
    $env.config.hooks.pre_execution = (
        $env.config.hooks.pre_execution
        | append {||
            let line = (commandline | str trim)
            if ($line | is-empty) {
                return
            }
            if ($line =~ '^lumina term(\s|$)') {
                return
            }
            if ($line =~ '^(@lumina|@g)(\s|$)') {
                return
            }
            job spawn { run-external "{lumina_bin}" "term" "log" $line | complete | ignore } | ignore
        }
    )
}
{command_not_found_handler}"#,
    command_not_found: Some(
        r#"
$env.config.hooks.command_not_found = {|command_name|
    let prompt = (try { commandline | str trim } catch { $command_name })
    print $"🪿 Command '($command_name)' not found. Asking lumina..."
    run-external "{lumina_bin}" "term" "run" $prompt | complete | ignore
    null
}"#,
    ),
};

static POWERSHELL_CONFIG: ShellConfig = ShellConfig {
    script_template: r#"$env:AGENT_SESSION_ID = "{session_id}"
function @lumina {{ & '{lumina_bin}' term run @args }}
function @g {{ & '{lumina_bin}' term run @args }}

Set-PSReadLineKeyHandler -Chord Enter -ScriptBlock {{
    $line = $null
    [Microsoft.PowerShell.PSConsoleReadLine]::GetBufferState([ref]$line, [ref]$null)
    if ($line -notmatch '^lumina term' -and $line -notmatch '^(@lumina|@g)($|\s)') {{
        Start-Job -ScriptBlock {{ & '{lumina_bin}' term log $using:line }} | Out-Null
    }}
    [Microsoft.PowerShell.PSConsoleReadLine]::AcceptLine()
}}"#,
    command_not_found: None,
};

fn render_term_init_script(
    shell: Shell,
    session_id: &str,
    lumina_bin: &str,
    with_command_not_found: bool,
) -> String {
    let config = shell.config();
    let command_not_found_handler = if with_command_not_found {
        config
            .command_not_found
            .map(|handler| handler.replace("{lumina_bin}", lumina_bin))
            .unwrap_or_default()
    } else {
        String::new()
    };

    config
        .script_template
        .replace("{session_id}", session_id)
        .replace("{lumina_bin}", lumina_bin)
        .replace("{command_not_found_handler}", &command_not_found_handler)
}

pub async fn handle_term_init(
    shell: Shell,
    name: Option<String>,
    with_command_not_found: bool,
) -> Result<()> {
    let session_manager = SessionManager::instance();

    let working_dir = std::env::current_dir()?;
    let named_session = if let Some(ref name) = name {
        let sessions = session_manager
            .list_sessions_by_types(&[SessionType::Terminal])
            .await?;
        sessions.into_iter().find(|s| s.name == *name)
    } else {
        None
    };

    let session = match named_session {
        Some(s) => s,
        None => {
            let session = session_manager
                .create_session(
                    working_dir,
                    "Lumina Term Session".to_string(),
                    SessionType::Terminal,
                    Config::global().get_lumina_mode().unwrap_or_default(),
                )
                .await?;

            if let Some(name) = name {
                session_manager
                    .update(&session.id)
                    .user_provided_name(name)
                    .apply()
                    .await?;
            }

            session
        }
    };

    let lumina_bin = std::env::current_exe()
        .map(|p| p.to_string_lossy().into_owned())
        .unwrap_or_else(|_| "lumina".to_string());

    println!(
        "{}",
        render_term_init_script(shell, &session.id, &lumina_bin, with_command_not_found)
    );
    Ok(())
}

pub async fn handle_term_log(command: String) -> Result<()> {
    let session_id = std::env::var("AGENT_SESSION_ID").map_err(|_| {
        anyhow!(
            "AGENT_SESSION_ID not set. Initialize terminal integration with `lumina term init <shell>` and reload your shell first."
        )
    })?;

    let message = Message::new(
        Role::User,
        chrono::Utc::now().timestamp_millis(),
        vec![MessageContent::text(command)],
    )
    .with_metadata(MessageMetadata::user_only())
    .with_generated_id();

    let session_manager = SessionManager::instance();
    session_manager.add_message(&session_id, &message).await?;

    Ok(())
}

pub async fn handle_term_run(prompt: Vec<String>) -> Result<()> {
    let prompt = prompt.join(" ");
    let session_id = std::env::var("AGENT_SESSION_ID").map_err(|_| {
        anyhow!(
            "AGENT_SESSION_ID not set.\n\n\
             Initialize terminal integration with `lumina term init <shell>` in your shell profile, \
             then restart or reload that shell."
        )
    })?;

    let working_dir = std::env::current_dir()?;
    let session_manager = SessionManager::instance();

    session_manager
        .update(&session_id)
        .working_dir(working_dir)
        .apply()
        .await?;

    let session = session_manager.get_session(&session_id, true).await?;
    let user_messages_after_last_assistant: Vec<&Message> =
        if let Some(conv) = &session.conversation {
            conv.messages()
                .iter()
                .rev()
                .take_while(|m| m.role != Role::Assistant)
                .collect()
        } else {
            Vec::new()
        };

    if let Some(oldest_user) = user_messages_after_last_assistant.last() {
        if let Some(message_id) = oldest_user.id.as_deref() {
            session_manager
                .truncate_conversation_from_message(&session_id, message_id)
                .await?;
        } else {
            session_manager
                .truncate_conversation(&session_id, oldest_user.created)
                .await?;
        }
    }

    let prompt_with_context = if user_messages_after_last_assistant.is_empty() {
        prompt
    } else {
        let history = user_messages_after_last_assistant
            .iter()
            .rev() // back to chronological order
            .map(|m| m.as_concat_text())
            .collect::<Vec<_>>()
            .join("\n");

        format!(
            "<shell_history>\n{}\n</shell_history>\n\n{}",
            history, prompt
        )
    };

    let config = SessionBuilderConfig {
        session_id: Some(session_id),
        resume: true,
        interactive: false,
        quiet: true,
        ..Default::default()
    };

    let mut session = build_session(config).await;
    session.headless(prompt_with_context).await?;

    Ok(())
}

/// Handle `lumina term info` - print compact session info for prompt integration
pub async fn handle_term_info() -> Result<()> {
    let session_id = match std::env::var("AGENT_SESSION_ID") {
        Ok(id) => id,
        Err(_) => return Ok(()),
    };

    let session_manager = SessionManager::instance();
    let session = session_manager.get_session(&session_id, false).await.ok();
    let total_tokens = session
        .as_ref()
        .and_then(|s| s.usage.total_tokens)
        .unwrap_or(0) as usize;

    let config = lumina::config::Config::global();
    let model_name = config
        .get_lumina_model()
        .ok()
        .map(|name| {
            let short = name.rsplit('/').next().unwrap_or(&name);
            if let Some(stripped) = short.strip_prefix("lumina-") {
                stripped.to_string()
            } else {
                short.to_string()
            }
        })
        .unwrap_or_else(|| "?".to_string());

    let context_limit = config
        .get_lumina_model()
        .ok()
        .and_then(|model_name| {
            config.get_lumina_provider().ok().and_then(|provider_name| {
                lumina::model_config::model_config_from_user_config(&provider_name, &model_name)
                    .ok()
            })
        })
        .map(|mc| mc.context_limit())
        .unwrap_or(128_000);

    let percentage = if context_limit > 0 {
        ((total_tokens as f64 / context_limit as f64) * 100.0).round() as usize
    } else {
        0
    };

    let filled = (percentage / 20).min(5);
    let empty = 5 - filled;
    let dots = format!("{}{}", "●".repeat(filled), "○".repeat(empty));

    println!("{} {}", dots, model_name);

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn render_term_init_script_includes_nushell_hooks() {
        let script = render_term_init_script(Shell::Nu, "session-123", "/tmp/lumina", false);

        assert!(script.contains("$env.AGENT_SESSION_ID = \"session-123\""));
        assert!(script.contains("def --wrapped @lumina [...args]"));
        assert!(script.contains("def --wrapped @g [...args]"));
        assert!(script.contains("LUMINA_NU_PREEXEC_INSTALLED"));
        assert!(script.contains("$env.config.hooks.pre_execution"));
        assert!(script.contains("job spawn { run-external \"/tmp/lumina\" \"term\" \"log\" $line | complete | ignore } | ignore"));
        assert!(!script.contains("command_not_found = {|command_name|"));
    }

    #[test]
    fn render_term_init_script_includes_nushell_default_handler() {
        let script = render_term_init_script(Shell::Nu, "session-123", "/tmp/lumina", true);

        assert!(script.contains("$env.config.hooks.command_not_found = {|command_name|"));
        assert!(script
            .contains("run-external \"/tmp/lumina\" \"term\" \"run\" $prompt | complete | ignore"));
    }

    #[test]
    fn render_term_init_script_skips_unsupported_default_handler() {
        let script = render_term_init_script(Shell::Fish, "session-123", "/tmp/lumina", true);

        assert!(!script.contains("command_not_found"));
    }
}
