use anyhow::Context;
use std::path::Path;

/// AGENTS.md template — the industry-standard agent instruction filename
/// (auto-detected by Claude Code / Codex / opencode / DeepseekNova). Simplified
/// skeleton of the repo-root AGENTS.md: 项目简介 / 常用命令 / 代码约定.
const AGENTS_MD_TEMPLATE: &str = r#"# {project_name} — 项目 Agent 指令

> **本文件是项目级 Agent 工作指令。`AGENTS.md` 是行业标准文件名，会被 Claude Code、Codex、opencode、DeepseekNova 等 AI 编程工具自动识别。在此项目中工作时，请优先遵守本文件约定。**

---

## 项目简介

[在此填写项目简介：项目目标、主要功能与技术栈]

## 常用命令

```bash
# 在此补充项目常用命令，例如：
make build          # 构建项目
make test           # 运行测试
make check          # 完整检查（fmt + lint + test）
```

## 代码约定

- [编码风格约定]
- [命名模式]
- [文件组织]
- 新功能必须附带测试

## 其他约定

- 自定义斜杠命令：`.deepseeknova/commands/*.md`
- 项目配置：`deepseeknova.toml`（可选）
"#;

/// Legacy DEEPSEEKNOVA.md template — kept for backward compatibility behind
/// `init --legacy`.
const LEGACY_TEMPLATE: &str = r#"# {project_name} — Project Context

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
Custom slash commands go in .deepseeknova/commands/ as .md files.
"#;

/// Initialize a new deepseeknova project in the current directory.
///
/// Default behavior generates the industry-standard `AGENTS.md` agent
/// instruction file. `legacy=true` (from `init --legacy`, see cli.rs) falls
/// back to the legacy private `DEEPSEEKNOVA.md`.
pub async fn run_init(legacy: bool) -> anyhow::Result<()> {
    let cwd = std::env::current_dir().context("cannot determine current directory")?;
    run_init_at(&cwd, legacy)
}

/// Core init implementation, testable against an arbitrary root directory.
/// `legacy=true` generates `DEEPSEEKNOVA.md`; otherwise `AGENTS.md`.
pub fn run_init_at(root: &Path, legacy: bool) -> anyhow::Result<()> {
    // Create .deepseeknova/commands/
    let commands_dir = root.join(".deepseeknova").join("commands");
    std::fs::create_dir_all(&commands_dir)
        .with_context(|| format!("failed to create {}", commands_dir.display()))?;

    // Create .deepseeknova/memory/
    let memory_dir = root.join(".deepseeknova").join("memory");
    std::fs::create_dir_all(&memory_dir)
        .with_context(|| format!("failed to create {}", memory_dir.display()))?;

    let project_name = root
        .file_name()
        .and_then(|n| n.to_str())
        .unwrap_or("my-project");

    // Agent instruction file: AGENTS.md by default, DEEPSEEKNOVA.md behind --legacy.
    let agent_file = if legacy {
        "DEEPSEEKNOVA.md"
    } else {
        "AGENTS.md"
    };
    let agent_path = root.join(agent_file);
    if !agent_path.exists() {
        let template = if legacy {
            LEGACY_TEMPLATE
        } else {
            AGENTS_MD_TEMPLATE
        };
        std::fs::write(
            &agent_path,
            template.replace("{project_name}", project_name),
        )
        .with_context(|| format!("failed to write {}", agent_path.display()))?;
        println!("✓ Created {}", agent_file);
    } else {
        println!("  {} already exists — skipping", agent_file);
    }

    // Create deepseeknova.toml if it doesn't exist
    let config_path = root.join("deepseeknova.toml");
    if !config_path.exists() {
        let template = r#"# deepseeknova project configuration

[agent]
max_steps = 25

[permissions]
default_mode = "ask"
"#;
        std::fs::write(&config_path, template)
            .with_context(|| format!("failed to write {}", config_path.display()))?;
        println!("✓ Created deepseeknova.toml");
    } else {
        println!("  deepseeknova.toml already exists — skipping");
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
    println!("✓ deepseeknova project initialized at {}", root.display());
    println!();
    println!("Next steps:");
    if legacy {
        println!("  1. Edit DEEPSEEKNOVA.md with your project context");
    } else {
        println!("  1. Edit AGENTS.md — add your project description, commands, and conventions");
    }
    println!("  2. Add custom commands to .deepseeknova/commands/");
    println!("  3. Run `deepseeknova-cli setup` to configure your LLM provider (first time)");
    println!("  4. Run `deepseeknova-cli chat --tui` to start an interactive session");
    println!("  5. Or run `deepseeknova-cli run \"<your first task>\"` for a one-shot task");
    println!();
    if !legacy {
        println!(
            "Tip: AGENTS.md is the industry-standard agent instructions file, auto-detected by"
        );
        println!("     Claude Code / Codex / opencode / DeepseekNova. For the legacy private");
        println!("     filename, re-run with `init --legacy`.");
    }

    Ok(())
}

/// Load custom slash commands from .deepseeknova/commands/*.md.
#[allow(dead_code)] // Will be wired into chat REPL in Phase 4 (slash commands / skills)
pub fn load_custom_commands(root: &Path) -> Vec<CustomCommand> {
    let commands_dir = root.join(".deepseeknova").join("commands");
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
        let dir =
            std::env::temp_dir().join(format!("deepseeknova-init-test-{}", std::process::id()));
        let commands_dir = dir.join(".deepseeknova").join("commands");
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
        let dir =
            std::env::temp_dir().join(format!("deepseeknova-init-empty-{}", std::process::id()));
        let commands_dir = dir.join(".deepseeknova").join("commands");
        std::fs::create_dir_all(&commands_dir).unwrap();

        let commands = load_custom_commands(&dir);
        let _ = std::fs::remove_dir_all(&dir);

        assert!(commands.is_empty());
    }

    #[test]
    fn init_creates_agents_md_by_default() {
        let dir = tempfile::tempdir().unwrap();
        run_init_at(dir.path(), false).unwrap();

        let agents_path = dir.path().join("AGENTS.md");
        assert!(agents_path.exists(), "default init must create AGENTS.md");
        let content = std::fs::read_to_string(&agents_path).unwrap();
        assert!(
            content.contains("项目简介"),
            "AGENTS.md template must include 项目简介 section"
        );
        assert!(
            content.contains("常用命令"),
            "AGENTS.md template must include 常用命令 section"
        );
        assert!(
            content.contains("代码约定"),
            "AGENTS.md template must include 代码约定 section"
        );

        // DEEPSEEKNOVA.md 降为可选：默认模式不得生成。
        assert!(
            !dir.path().join("DEEPSEEKNOVA.md").exists(),
            "default init must not create DEEPSEEKNOVA.md"
        );

        // 其余脚手架照常生成。
        assert!(dir.path().join("deepseeknova.toml").exists());
        assert!(dir.path().join(".deepseeknova").join("memory").is_dir());
        assert!(dir
            .path()
            .join(".deepseeknova")
            .join("commands")
            .join("build.md")
            .exists());
    }

    #[test]
    fn init_skips_when_agents_md_exists() {
        let dir = tempfile::tempdir().unwrap();
        let agents_path = dir.path().join("AGENTS.md");
        std::fs::write(&agents_path, "# existing content\n").unwrap();

        run_init_at(dir.path(), false).unwrap();

        let content = std::fs::read_to_string(&agents_path).unwrap();
        assert_eq!(
            content, "# existing content\n",
            "existing AGENTS.md must be left untouched"
        );
    }

    #[test]
    fn init_legacy_creates_deepseeknova_md() {
        let dir = tempfile::tempdir().unwrap();
        run_init_at(dir.path(), true).unwrap();

        assert!(
            dir.path().join("DEEPSEEKNOVA.md").exists(),
            "legacy init must create DEEPSEEKNOVA.md"
        );
        assert!(
            !dir.path().join("AGENTS.md").exists(),
            "legacy init must not create AGENTS.md"
        );
        let content = std::fs::read_to_string(dir.path().join("DEEPSEEKNOVA.md")).unwrap();
        assert!(content.contains("Overview"), "legacy template kept intact");
    }

    #[test]
    fn init_legacy_skips_existing_deepseeknova_md() {
        let dir = tempfile::tempdir().unwrap();
        let legacy_path = dir.path().join("DEEPSEEKNOVA.md");
        std::fs::write(&legacy_path, "# keep\n").unwrap();

        run_init_at(dir.path(), true).unwrap();

        assert_eq!(
            std::fs::read_to_string(&legacy_path).unwrap(),
            "# keep\n",
            "existing DEEPSEEKNOVA.md must be left untouched"
        );
    }
}
