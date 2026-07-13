use reasonix_core::runner::{RunEvent, RunInput, Runner};
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

/// Run an interactive chat REPL session with rich slash commands.
pub async fn run_chat_repl(
    runner: &dyn Runner,
    model_override: Option<String>,
) -> anyhow::Result<bool> {
    println!();
    println!("╭──────────────────────────────────────────────────╮");
    println!("│     reasonix — interactive chat                  │");
    println!("│     /exit  /new  /model  /skills  /help          │");
    println!("╰──────────────────────────────────────────────────╯");
    println!();

    let stdin = std::io::stdin();
    let mut reader = std::io::BufReader::new(stdin.lock());
    let mut mode = DisplayMode::Normal;

    let mut restart_requested = false;

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
            let should_break = handle_slash_command(cmd, &mut mode, &mut restart_requested).await?;
            if should_break {
                break;
            }
            continue;
        }

        // Send to agent
        let input = RunInput {
            prompt: trimmed.to_string(),
            images: Vec::new(),
            model_override: model_override.clone(),
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

/// Handle a slash command. Returns `Ok(true)` if the caller should break the loop.
/// Sets `restart_requested` to true if a new session should be created.
async fn handle_slash_command(
    cmd: &str,
    mode: &mut DisplayMode,
    restart_requested: &mut bool,
) -> anyhow::Result<bool> {
    // Split command and optional arguments
    let (name, _args) = cmd.split_once(' ').unwrap_or((cmd, ""));

    match name {
        // ── Exit ──────────────────────────────────────────────
        "exit" | "quit" | "q" => {
            println!("goodbye.");
            return Ok(true);
        }

        // ── New session ───────────────────────────────────────
        "new" => {
            println!("starting a new session...");
            *restart_requested = true;
            return Ok(true); // caller recreates the runner
        }

        // ── Clear screen ──────────────────────────────────────
        "clear" => {
            print!("\x1b[2J\x1b[H");
        }

        // ── Display mode ──────────────────────────────────────
        "raw" => {
            match mode {
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
            }
        }

        // ── Model info ────────────────────────────────────────
        "model" => {
            println!("Model commands:");
            println!("  /model          — show this help");
            println!("  /model switch   — switch model (not yet wired)");
            println!("  /model thinking — toggle thinking mode (not yet wired)");
            println!("  /model effort   — set reasoning effort low|medium|high");
            println!();
            println!("Current: configured via reasonix.toml or --model flag");
        }

        // ── Skills ────────────────────────────────────────────
        "skills" => {
            // Try to load skills from standard paths
            let paths = [
                ".reasonix/skills",
                ".agents/skills",
            ];
            let mut found = false;
            for path_str in &paths {
                let loader = reasonix_skills::SkillLoader::new(path_str);
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
                println!("No skills found. Create .md files in .reasonix/skills/");
            }
        }

        // ── MCP status ────────────────────────────────────────
        "mcp" => {
            println!("MCP servers are configured in reasonix.toml:");
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
            println!("Use the checkpoint system: crates/reasonix-checkpoint");
        }

        // ── Help ──────────────────────────────────────────────
        "help" => {
            println!("Commands:");
            println!("  /exit, /quit, /q  — end the session");
            println!("  /new              — start a new conversation");
            println!("  /clear            — clear the screen");
            println!("  /raw              — cycle display mode (normal/lite/raw)");
            println!("  /model            — show model configuration");
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

    Ok(false)
}

fn truncate(s: &str, max: usize) -> String {
    if s.len() <= max {
        s.to_string()
    } else {
        let end = s.floor_char_boundary(max);
        format!("{}…", &s[..end])
    }
}
