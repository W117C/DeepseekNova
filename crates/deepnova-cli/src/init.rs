use anyhow::Context;
use std::path::Path;

/// Initialize a new deepnova project in the current directory.
/// Creates: DPRONIX.md, deepnova.toml (if not exists),
/// .deepnova/commands/ (empty dir), .deepnova/memory/ (empty dir).
pub async fn run_init() -> anyhow::Result<()> {
    let cwd = std::env::current_dir().context("cannot determine current directory")?;

    // Create .deepnova/commands/
    let commands_dir = cwd.join(".deepnova").join("commands");
    std::fs::create_dir_all(&commands_dir)
        .with_context(|| format!("failed to create {}", commands_dir.display()))?;

    // Create .deepnova/memory/
    let memory_dir = cwd.join(".deepnova").join("memory");
    std::fs::create_dir_all(&memory_dir)
        .with_context(|| format!("failed to create {}", memory_dir.display()))?;

    // Create DPRONIX.md if it doesn't exist
    let deepnova_md_path = cwd.join("DPRONIX.md");
    if !deepnova_md_path.exists() {
        let project_name = cwd
            .file_name()
            .and_then(|n| n.to_str())
            .unwrap_or("my-project");

        let template = format!(
            r#"# {project_name} — Project Context

## Overview
[Brief description of what this project does]

## Tech Stack
- [Language / runtime]
- [Key libraries]

## Architecture
[High-level architecture notes]

## Conventions
- [Coding conventions specific to this project]
- [Naming patterns]
- [File organization]

## Commands
Custom slash commands go in .deepnova/commands/ as .md files.
"#
        );
        std::fs::write(&deepnova_md_path, template)
            .with_context(|| format!("failed to write {}", deepnova_md_path.display()))?;
        println!("✓ Created DPRONIX.md");
    } else {
        println!("  DPRONIX.md already exists — skipping");
    }

    // Create deepnova.toml if it doesn't exist
    let config_path = cwd.join("deepnova.toml");
    if !config_path.exists() {
        let template = r#"# deepnova project configuration

[agent]
max_steps = 10

[permissions]
default_mode = "ask"
"#;
        std::fs::write(&config_path, template)
            .with_context(|| format!("failed to write {}", config_path.display()))?;
        println!("✓ Created deepnova.toml");
    } else {
        println!("  deepnova.toml already exists — skipping");
    }

    // Create a sample command
    let sample_cmd = commands_dir.join("build.md");
    if !sample_cmd.exists() {
        let sample = r#"---
description: Build the project
---
Run the project build command and report any errors.
"#;
        std::fs::write(&sample_cmd, sample)?;
    }

    println!();
    println!("deepnova project initialized at {}", cwd.display());
    println!();
    println!("Next steps:");
    println!("  1. Edit DPRONIX.md with your project context");
    println!("  2. Add custom commands to .deepnova/commands/");
    println!("  3. Run `deepnova chat` to start a session");

    Ok(())
}

/// Load custom slash commands from .deepnova/commands/*.md.
#[allow(dead_code)] // Will be wired into chat REPL in Phase 4 (slash commands / skills)
pub fn load_custom_commands(root: &Path) -> Vec<CustomCommand> {
    let commands_dir = root.join(".deepnova").join("commands");
    if !commands_dir.is_dir() {
        return Vec::new();
    }

    let mut commands = Vec::new();
    if let Ok(entries) = std::fs::read_dir(&commands_dir) {
        for entry in entries.flatten() {
            let path = entry.path();
            if path.extension().is_none_or(|e| e != "md") {
                continue;
            }

            if let Ok(content) = std::fs::read_to_string(&path) {
                if let Some(cmd) = parse_command_md(&content) {
                    let name = path
                        .file_stem()
                        .and_then(|s| s.to_str())
                        .unwrap_or("unknown")
                        .to_string();
                    commands.push(CustomCommand { name, ..cmd });
                }
            }
        }
    }
    commands
}

#[derive(Debug, Clone)]
#[allow(dead_code)]
pub struct CustomCommand {
    pub name: String,
    pub description: String,
    pub body: String,
}

/// Parse a command markdown file with optional frontmatter.
fn parse_command_md(content: &str) -> Option<CustomCommand> {
    let body = if let Some(rest) = content.strip_prefix("---") {
        // Has YAML frontmatter
        if let Some(end) = rest.find("---") {
            let fm = &rest[..end];
            let body = rest[end + 3..].trim().to_string();

            // Very simple frontmatter parsing
            let description = fm
                .lines()
                .find_map(|line| {
                    let line = line.trim();
                    line.strip_prefix("description:")
                        .map(|d| d.trim().trim_matches('"').to_string())
                })
                .unwrap_or_default();

            return Some(CustomCommand {
                name: String::new(), // filled by caller
                description,
                body,
            });
        }
        return None;
    } else {
        content.to_string()
    };

    Some(CustomCommand {
        name: String::new(),
        description: String::new(),
        body,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_command_with_frontmatter() {
        let content = "---\ndescription: \"Build the project\"\n---\nRun the build command.";
        let cmd = parse_command_md(content).unwrap();
        assert_eq!(cmd.description, "Build the project");
        assert_eq!(cmd.body, "Run the build command.");
        assert!(cmd.name.is_empty()); // filled by caller
    }

    #[test]
    fn parse_command_without_frontmatter() {
        let content = "Just run the tests and report results.";
        let cmd = parse_command_md(content).unwrap();
        assert!(cmd.description.is_empty());
        assert_eq!(cmd.body, "Just run the tests and report results.");
    }

    #[test]
    fn parse_command_with_extra_whitespace_in_frontmatter() {
        let content = "---\ndescription:   \"Lint the codebase\"   \n---\nRun clippy.";
        let cmd = parse_command_md(content).unwrap();
        assert_eq!(cmd.description, "Lint the codebase");
        assert_eq!(cmd.body, "Run clippy.");
    }

    #[test]
    fn parse_command_missing_closing_frontmatter() {
        let content = "---\ndescription: \"incomplete\"\nRun something.";
        assert!(parse_command_md(content).is_none());
    }

    #[test]
    fn load_commands_from_temp_dir() {
        let dir = std::env::temp_dir().join(format!("deepnova-init-test-{}", std::process::id()));
        let commands_dir = dir.join(".deepnova").join("commands");
        std::fs::create_dir_all(&commands_dir).unwrap();

        // Write a valid command file
        let cmd_path = commands_dir.join("test-cmd.md");
        std::fs::write(
            &cmd_path,
            "---\ndescription: \"A test command\"\n---\nExecute the test.",
        )
        .unwrap();

        let commands = load_custom_commands(&dir);
        // Cleanup
        let _ = std::fs::remove_dir_all(&dir);

        assert_eq!(commands.len(), 1);
        assert_eq!(commands[0].name, "test-cmd");
        assert_eq!(commands[0].description, "A test command");
        assert_eq!(commands[0].body, "Execute the test.");
    }

    #[test]
    fn load_commands_empty_dir() {
        let dir = std::env::temp_dir().join(format!("deepnova-init-empty-{}", std::process::id()));
        let commands_dir = dir.join(".deepnova").join("commands");
        std::fs::create_dir_all(&commands_dir).unwrap();

        let commands = load_custom_commands(&dir);
        let _ = std::fs::remove_dir_all(&dir);

        assert!(commands.is_empty());
    }
}
