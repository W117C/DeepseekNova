use reasonix_core::runner::{RunEvent, RunInput, Runner};
use std::io::{BufRead, Write};
use tokio_stream::StreamExt;

/// Run an interactive chat REPL session.
pub async fn run_chat_repl(
    runner: &dyn Runner,
    model_override: Option<String>,
) -> anyhow::Result<()> {
    println!();
    println!("╭────────────────────────────────────────╮");
    println!("│     reasonix — interactive chat        │");
    println!("│     /exit  /clear  /compact /help      │");
    println!("╰────────────────────────────────────────╯");
    println!();

    let stdin = std::io::stdin();
    let mut reader = std::io::BufReader::new(stdin.lock());

    loop {
        // Prompt
        print!("> ");
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
            match cmd {
                "exit" | "quit" | "q" => {
                    println!("goodbye.");
                    break;
                }
                "clear" => {
                    // Clear screen
                    print!("\x1b[2J\x1b[H");
                    continue;
                }
                "compact" => {
                    // Compaction is automatic when compaction_threshold_tokens is set.
                    // A manual trigger can be added if Agent exposes a compact() method.
                    println!("compaction is handled automatically based on token threshold.");
                    continue;
                }
                "help" => {
                    println!("Commands:");
                    println!("  /exit, /quit, /q  — end the session");
                    println!("  /clear            — clear the screen");
                    println!("  /compact          — request conversation compaction");
                    println!("  /help             — show this help");
                    println!();
                    println!("Anything else is sent to the agent as a prompt.");
                    continue;
                }
                other => {
                    eprintln!("unknown command: /{other}");
                    continue;
                }
            }
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
                            print!("{text}");
                            std::io::stdout().flush().ok();
                        }
                        Ok(RunEvent::ToolCallStart { name, .. }) => {
                            if started_output {
                                println!();
                                started_output = false;
                            }
                            print!("  ⚙ {name} ...");
                            std::io::stdout().flush().ok();
                        }
                        Ok(RunEvent::ToolCallEnd {
                            name: _, arguments, ..
                        }) => {
                            println!();
                            println!("     args: {}", truncate(&arguments, 200));
                        }
                        Ok(RunEvent::ToolResult { call_id: _, result }) => {
                            println!("     → {}", truncate(&result, 300));
                        }
                        Ok(RunEvent::Usage(u)) => {
                            if started_output {
                                println!();
                                started_output = false;
                            }
                            eprintln!(
                                "  [{}↑ {}↓ {} total]",
                                u.prompt_tokens, u.completion_tokens, u.total_tokens
                            );
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

    Ok(())
}

fn truncate(s: &str, max: usize) -> String {
    if s.len() <= max {
        s.to_string()
    } else {
        format!("{}…", &s[..max])
    }
}
