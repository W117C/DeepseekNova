use dpronix_core::runner::{RunEvent, RunInput, Runner};
use dpronix_provider::factory::ReasoningEffort;
use std::io::{BufRead, Write};
use tokio_stream::StreamExt;

/// Display mode for agent output.
#[derive(Clone, Copy, PartialEq, Eq)]
enum DisplayMode {
    /// Show text, reasoning, tool calls, results — everything.
    Normal,
    /// Show only text and tool results (hide reasoning).
    Lite,
    /// Show raw chunk types without formatting.
    Raw,
}

/// What the slash-command handler wants the main loop to do.
#[derive(Clone)]
enum SlashAction {
    Continue,
    Break,
    /// Rebuild the agent with a new reasoning effort and/or model.
    Rebuild {
        effort: Option<ReasoningEffort>,
        model: Option<String>,
    },
}

/// Run an interactive chat REPL session with rich slash commands.
///
/// `agent_factory` is called to (re-)create the agent when the session
/// starts or when the user changes model/reasoning-effort via `/model`
/// commands.  The factory receives the resolved effort level and an optional
/// model override and must return a boxed [`Runner`] + [`Send`].
pub async fn run_chat_repl<F>(
    agent_factory: F,
    baseline_effort: ReasoningEffort,
    initial_model: Option<String>,
) -> anyhow::Result<bool>
where
    F: Fn(Option<ReasoningEffort>, Option<String>) -> anyhow::Result<Box<dyn Runner + Send>>,
{
    let mut current_effort = baseline_effort;
    let mut current_model = initial_model;
    println!();
    println!("╭──────────────────────────────────────────────────╮");
    println!("│     dpronix — interactive chat                  │");
    println!("│     /exit  /new  /model  /skills  /help          │");
    println!("╰──────────────────────────────────────────────────╯");
    println!();

    let stdin = std::io::stdin();
    let mut reader = std::io::BufReader::new(stdin.lock());
    let mut mode = DisplayMode::Normal;

    let mut restart_requested = false;

    // Build initial agent via the factory.
    let mut runner = agent_factory(Some(current_effort), current_model.clone())?;

    loop {
        // Prompt
        let mode_indicator = match mode {
            DisplayMode::Normal => "",
            DisplayMode::Lite => " [lite]",
            DisplayMode::Raw => " [raw]",
        };
        print!(">{mode_indicator} ");
        std::io::stdout().flush().ok();

        let mut line = String::new();
        match reader.read_line(&mut line) {
            Ok(0) => {
                // EOF — exit
                println!();
                break;
            }
            Ok(_) => {}
            Err(e) => {
                eprintln!("read error: {e}");
                break;
            }
        }

        let trimmed = line.trim();

        // Empty input — skip
        if trimmed.is_empty() {
            continue;
        }

        // Slash commands
        if let Some(cmd) = trimmed.strip_prefix('/') {
            let action = handle_slash_command(
                cmd,
                &mut mode,
                &mut restart_requested,
                &mut current_effort,
                baseline_effort,
                &mut current_model,
            )
            .await?;
            match action {
                SlashAction::Break => break,
                SlashAction::Rebuild { effort, model } => {
                    // Merge: keep existing values unless command specifies new ones.
                    if let Some(e) = effort {
                        current_effort = e;
                    }
                    if let Some(m) = model {
                        current_model = Some(m);
                    }
                    match agent_factory(Some(current_effort), current_model.clone()) {
                        Ok(new_runner) => {
                            println!(
                                "switched: effort={effort_display}, model={model_display}",
                                effort_display = effort_label(current_effort),
                                model_display = current_model.as_deref().unwrap_or("(default)")
                            );
                            runner = new_runner;
                        }
                        Err(e) => {
                            eprintln!("failed to rebuild agent: {e}");
                        }
                    }
                }
                SlashAction::Continue => {}
            }
            continue;
        }

        // Send to agent
        let input = RunInput {
            prompt: trimmed.to_string(),
            images: Vec::new(),
            model_override: current_model.clone(),
        };

        match runner.run_stream(input).await {
            Ok(mut stream) => {
                println!();
                let mut started_output = false;

                while let Some(event) = stream.next().await {
                    match event {
                        Ok(RunEvent::TextDelta(text)) => {
                            if !started_output {
                                started_output = true;
                            }
                            if mode == DisplayMode::Raw {
                                println!("[text] {text}");
                            } else {
                                print!("{text}");
                            }
                            std::io::stdout().flush().ok();
                        }
                        Ok(RunEvent::ReasoningDelta { text, .. }) => {
                            if mode == DisplayMode::Raw {
                                println!("[reasoning] {text}");
                            } else if mode == DisplayMode::Normal {
                                // Show reasoning in dim style
                                print!("\x1b[2m{text}\x1b[0m");
                                std::io::stdout().flush().ok();
                            }
                            // Lite mode: hide reasoning
                        }
                        Ok(RunEvent::ToolCallStart { name, .. }) => {
                            if mode == DisplayMode::Raw {
                                println!("[tool_start] {name}");
                            } else {
                                if started_output {
                                    println!();
                                    started_output = false;
                                }
                                print!("  ⚙ {name} ...");
                            }
                            std::io::stdout().flush().ok();
                        }
                        Ok(RunEvent::ToolCallEnd {
                            name: _, arguments, ..
                        }) => {
                            if mode != DisplayMode::Raw {
                                println!();
                            }
                            if mode == DisplayMode::Raw {
                                println!("[tool_end] args={}", truncate(&arguments, 200));
                            } else {
                                println!("     args: {}", truncate(&arguments, 200));
                            }
                        }
                        Ok(RunEvent::ToolResult { call_id: _, result }) => {
                            if mode == DisplayMode::Raw {
                                println!("[tool_result] {}", truncate(&result, 300));
                            } else {
                                println!("     → {}", truncate(&result, 300));
                            }
                        }
                        Ok(RunEvent::Usage(u)) => {
                            if started_output {
                                println!();
                                started_output = false;
                            }
                            if mode == DisplayMode::Raw {
                                println!(
                                    "[usage] {}↑ {}↓ (cache hit:{} miss:{})",
                                    u.prompt_tokens,
                                    u.completion_tokens,
                                    u.cache_hit_tokens,
                                    u.cache_miss_tokens
                                );
                            } else {
                                eprintln!(
                                    "  [{}↑ {}↓ {} total]",
                                    u.prompt_tokens, u.completion_tokens, u.total_tokens
                                );
                            }
                        }
                        Ok(RunEvent::Done(output)) => {
                            if started_output {
                                println!();
                            }
                            if !output.text.is_empty() {
                                println!("{}", output.text);
                            }
                        }
                        Ok(RunEvent::TurnComplete) if started_output => {
                            println!();
                            started_output = false;
                        }
                        Err(e) => {
                            eprintln!("\nerror: {e}");
                            break;
                        }
                        _ => {}
                    }
                }
                println!();
            }
            Err(e) => {
                eprintln!("error: {e}");
            }
        }
    }

    Ok(restart_requested)
}

/// Handle a slash command. Returns a [`SlashAction`] telling the main loop what to do.
async fn handle_slash_command(
    cmd: &str,
    mode: &mut DisplayMode,
    restart_requested: &mut bool,
    current_effort: &mut ReasoningEffort,
    baseline_effort: ReasoningEffort,
    current_model: &mut Option<String>,
) -> anyhow::Result<SlashAction> {
    // Split command and optional arguments
    let (name, args) = cmd.split_once(' ').unwrap_or((cmd, ""));

    match name {
        // ── Exit ──────────────────────────────────────────────
        "exit" | "quit" | "q" => {
            println!("goodbye.");
            return Ok(SlashAction::Break);
        }

        // ── New session ───────────────────────────────────────
        "new" => {
            println!("starting a new session...");
            *restart_requested = true;
            return Ok(SlashAction::Break); // caller recreates the runner
        }

        // ── Clear screen ──────────────────────────────────────
        "clear" => {
            print!("\x1b[2J\x1b[H");
        }

        // ── Display mode ──────────────────────────────────────
        "raw" => match mode {
            DisplayMode::Normal => {
                *mode = DisplayMode::Lite;
                println!("display mode: lite (hiding reasoning content)");
            }
            DisplayMode::Lite => {
                *mode = DisplayMode::Raw;
                println!("display mode: raw (showing chunk types)");
            }
            DisplayMode::Raw => {
                *mode = DisplayMode::Normal;
                println!("display mode: normal");
            }
        },

        // ── Model info & control ──────────────────────────────
        "model" => {
            let (sub, sub_args) = args.split_once(' ').unwrap_or((args, ""));
            match sub {
                "" | "help" => {
                    println!("Model commands:");
                    println!("  /model                — show this help");
                    println!(
                        "  /model effort <level>  — set reasoning effort: \
                         disabled|low|medium|high|max"
                    );
                    println!("  /model thinking        — toggle thinking on/off");
                    println!("  /model switch <name>   — switch to a named provider model");
                    println!();
                    println!(
                        "Current: effort={}, model={}",
                        effort_label(*current_effort),
                        current_model.as_deref().unwrap_or("(default)")
                    );
                }
                "effort" => {
                    if sub_args.is_empty() {
                        println!(
                            "Current reasoning effort: {} (config baseline: {})",
                            effort_label(*current_effort),
                            effort_label(baseline_effort)
                        );
                        println!("Usage: /model effort disabled|low|medium|high|max");
                    } else {
                        match parse_effort_command(sub_args) {
                            Ok(effort) => {
                                return Ok(SlashAction::Rebuild {
                                    effort: Some(effort),
                                    model: None,
                                });
                            }
                            Err(msg) => {
                                eprintln!("invalid effort level: {msg}");
                            }
                        }
                    }
                }
                "thinking" => {
                    let new_effort = toggle_thinking(*current_effort, baseline_effort);
                    println!(
                        "thinking {} → {}",
                        if current_effort.thinking() {
                            "on"
                        } else {
                            "off"
                        },
                        if new_effort.thinking() { "on" } else { "off" }
                    );
                    if new_effort != *current_effort {
                        return Ok(SlashAction::Rebuild {
                            effort: Some(new_effort),
                            model: None,
                        });
                    }
                }
                "switch" => {
                    if sub_args.is_empty() {
                        eprintln!("Usage: /model switch <provider-model-name>");
                    } else {
                        return Ok(SlashAction::Rebuild {
                            effort: None,
                            model: Some(sub_args.to_string()),
                        });
                    }
                }
                other => {
                    eprintln!("unknown /model sub-command: {other}");
                    eprintln!("try /model help");
                }
            }
        }

        // ── Skills ────────────────────────────────────────────
        "skills" => {
            // Try to load skills from standard paths
            let paths = [".dpronix/skills", ".agents/skills"];
            let mut found = false;
            for path_str in &paths {
                let loader = dpronix_skills::SkillLoader::new(path_str);
                match loader.load_all() {
                    Ok(skills) if !skills.is_empty() => {
                        if !found {
                            println!("Available skills:");
                            found = true;
                        }
                        for skill in &skills {
                            println!("  • {} — {}", skill.name, skill.description);
                            if !skill.tools_allowed.is_empty() {
                                println!("    tools: {}", skill.tools_allowed.join(", "));
                            }
                        }
                    }
                    Ok(_) => {}
                    Err(e) => {
                        eprintln!("error loading skills from {path_str}: {e}");
                    }
                }
            }
            if !found {
                println!("No skills found. Create .md files in .dpronix/skills/");
            }
        }

        // ── MCP status ────────────────────────────────────────
        "mcp" => {
            println!("MCP servers are configured in dpronix.toml:");
            println!("  [[mcp_servers]]");
            println!("  name = \"my-server\"");
            println!("  command = \"npx\"");
            println!("  args = [\"-y\", \"@modelcontextprotocol/server-filesystem\"]");
            println!();
            println!("Use /mcp status to check connected servers (coming soon).");
        }

        // ── Undo ──────────────────────────────────────────────
        "undo" => {
            println!("Undo is not yet implemented in the CLI.");
            println!("Use the checkpoint system: crates/dpronix-checkpoint");
        }

        // ── Help ──────────────────────────────────────────────
        "help" => {
            println!("Commands:");
            println!("  /exit, /quit, /q  — end the session");
            println!("  /new              — start a new conversation");
            println!("  /clear            — clear the screen");
            println!("  /raw              — cycle display mode (normal/lite/raw)");
            println!("  /model            — show / change model & reasoning settings");
            println!("  /skills           — list available agent skills");
            println!("  /mcp              — MCP server status");
            println!("  /undo             — revert changes (coming soon)");
            println!("  /help             — show this help");
            println!();
            println!("Display modes:");
            println!("  normal  — text, reasoning, tool calls, results");
            println!("  lite    — hide reasoning content");
            println!("  raw     — show chunk types");
            println!();
            println!("Anything else is sent to the agent as a prompt.");
        }

        // ── Unknown ───────────────────────────────────────────
        other => {
            eprintln!("unknown command: /{other}");
            eprintln!("type /help for available commands.");
        }
    }

    Ok(SlashAction::Continue)
}

// ---------------------------------------------------------------------------
// Pure helpers (testable)
// ---------------------------------------------------------------------------

/// Parse a user-supplied reasoning-effort argument string into a
/// [`ReasoningEffort`].  Returns `Err(msg)` when the input isn't recognised.
fn parse_effort_command(args: &str) -> Result<ReasoningEffort, String> {
    let trimmed = args.trim();
    if trimmed.is_empty() {
        return Err("no level provided".into());
    }
    ReasoningEffort::from_config_str(trimmed)
        .ok_or_else(|| format!("unknown effort level: '{trimmed}'"))
}

/// Toggle thinking on/off: if currently enabled → disable; if disabled →
/// restore the baseline.  Always returns a new [`ReasoningEffort`]; the
/// caller decides whether to rebuild.
fn toggle_thinking(current: ReasoningEffort, baseline: ReasoningEffort) -> ReasoningEffort {
    if current.thinking() {
        ReasoningEffort::Disabled
    } else {
        baseline
    }
}

/// Human-readable label for a reasoning-effort level.
fn effort_label(effort: ReasoningEffort) -> &'static str {
    match effort {
        ReasoningEffort::Disabled => "disabled",
        ReasoningEffort::High => "high",
        ReasoningEffort::Max => "max",
    }
}

fn truncate(s: &str, max: usize) -> String {
    if s.len() <= max {
        s.to_string()
    } else {
        let end = s.floor_char_boundary(max);
        format!("{}…", &s[..end])
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    // ── parse_effort_command ───────────────────────────────────────────

    #[test]
    fn parse_effort_known_levels() {
        assert_eq!(
            parse_effort_command("disabled").unwrap(),
            ReasoningEffort::Disabled
        );
        assert_eq!(
            parse_effort_command("off").unwrap(),
            ReasoningEffort::Disabled
        );
        assert_eq!(
            parse_effort_command("none").unwrap(),
            ReasoningEffort::Disabled
        );
        assert_eq!(
            parse_effort_command("false").unwrap(),
            ReasoningEffort::Disabled
        );

        assert_eq!(parse_effort_command("high").unwrap(), ReasoningEffort::High);
        assert_eq!(
            parse_effort_command("medium").unwrap(),
            ReasoningEffort::High
        );
        assert_eq!(parse_effort_command("low").unwrap(), ReasoningEffort::High);

        assert_eq!(parse_effort_command("max").unwrap(), ReasoningEffort::Max);
        assert_eq!(
            parse_effort_command("maximum").unwrap(),
            ReasoningEffort::Max
        );
    }

    #[test]
    fn parse_effort_with_whitespace() {
        assert_eq!(
            parse_effort_command("  high  ").unwrap(),
            ReasoningEffort::High
        );
    }

    #[test]
    fn parse_effort_empty() {
        assert!(parse_effort_command("").is_err());
        assert!(parse_effort_command("   ").is_err());
    }

    #[test]
    fn parse_effort_unknown() {
        assert!(parse_effort_command("ultra").is_err());
        assert!(parse_effort_command("x-high").is_err());
    }

    // ── toggle_thinking ─────────────────────────────────────────────────

    #[test]
    fn toggle_thinking_disables_when_enabled() {
        assert_eq!(
            toggle_thinking(ReasoningEffort::High, ReasoningEffort::High),
            ReasoningEffort::Disabled
        );
        assert_eq!(
            toggle_thinking(ReasoningEffort::Max, ReasoningEffort::High),
            ReasoningEffort::Disabled
        );
    }

    #[test]
    fn toggle_thinking_restores_baseline_when_disabled() {
        assert_eq!(
            toggle_thinking(ReasoningEffort::Disabled, ReasoningEffort::High),
            ReasoningEffort::High
        );
        assert_eq!(
            toggle_thinking(ReasoningEffort::Disabled, ReasoningEffort::Max),
            ReasoningEffort::Max
        );
    }

    #[test]
    fn toggle_thinking_noop_when_baseline_is_disabled() {
        // Toggle off: disabled → disabled
        assert_eq!(
            toggle_thinking(ReasoningEffort::Disabled, ReasoningEffort::Disabled),
            ReasoningEffort::Disabled
        );
    }

    // ── effort_label ────────────────────────────────────────────────────

    #[test]
    fn effort_label_values() {
        assert_eq!(effort_label(ReasoningEffort::Disabled), "disabled");
        assert_eq!(effort_label(ReasoningEffort::High), "high");
        assert_eq!(effort_label(ReasoningEffort::Max), "max");
    }
}
