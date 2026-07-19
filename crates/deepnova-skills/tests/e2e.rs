//! End-to-end tests for the Skills system — load from real files, wrap as tools,
//! and execute.

use deepnova_core::registry::Skill;
use deepnova_core::{Tool, ToolContext};
use deepnova_skills::{SkillLoader, SkillTool};
use std::sync::Arc;
use tempfile::TempDir;

#[test]
fn e2e_load_skills_from_directory() {
    let dir = TempDir::new().unwrap();
    let skills_dir = dir.path().join("skills");
    std::fs::create_dir_all(&skills_dir).unwrap();

    // Write a valid skill file
    std::fs::write(
        skills_dir.join("reviewer.md"),
        "---\nname: code-reviewer\ndescription: Review code\ntools_allowed:\n  - read_file\n---\n\nBe thorough and check for bugs.",
    )
    .unwrap();

    // Write another skill
    std::fs::write(
        skills_dir.join("tester.md"),
        "---\nname: tester\ndescription: Write tests\n---\n\nWrite comprehensive tests.",
    )
    .unwrap();

    let loader = SkillLoader::new(&skills_dir);
    let skills = loader.load_all().unwrap();

    assert_eq!(skills.len(), 2);
    let names: Vec<&str> = skills.iter().map(|s| s.name.as_str()).collect();
    assert!(names.contains(&"code-reviewer"));
    assert!(names.contains(&"tester"));
}

#[tokio::test]
async fn e2e_skill_to_tool_roundtrip() {
    let dir = TempDir::new().unwrap();
    let skills_dir = dir.path().join("skills");
    std::fs::create_dir_all(&skills_dir).unwrap();

    std::fs::write(
        skills_dir.join("planner.md"),
        "---\nname: planner\ndescription: Plan tasks\nmodel: claude-opus-4-8\ntools_allowed:\n  - grep\n  - glob\n---\n\nYou are a planning assistant. Break down complex tasks.",
    )
    .unwrap();

    let loader = SkillLoader::new(&skills_dir);
    let skills = loader.load_all().unwrap();
    assert_eq!(skills.len(), 1);

    let tool = SkillTool::new(skills.into_iter().next().unwrap());
    let schema = tool.schema();

    // Schema name is prefixed
    assert_eq!(schema.name, "skill__planner");
    assert!(schema.description.contains("planner"));

    // Execute returns the prompt
    let ctx = ToolContext::new("call-1");
    let result = tool.execute(&ctx, "{}").await.unwrap();

    assert!(result.contains("Skill Activated: planner"));
    assert!(result.contains("You are a planning assistant"));
    assert!(result.contains("grep"));
    assert!(result.contains("glob"));
    assert!(result.contains("claude-opus-4-8"));
}

#[test]
fn e2e_non_md_files_are_skipped() {
    let dir = TempDir::new().unwrap();
    let skills_dir = dir.path().join("skills");
    std::fs::create_dir_all(&skills_dir).unwrap();

    std::fs::write(skills_dir.join("notes.txt"), "not a skill").unwrap();
    std::fs::write(
        skills_dir.join("real.md"),
        "---\nname: real\ndescription: A skill\n---\n\nReal body.",
    )
    .unwrap();

    let loader = SkillLoader::new(&skills_dir);
    let skills = loader.load_all().unwrap();

    assert_eq!(skills.len(), 1);
    assert_eq!(skills[0].name, "real");
}

#[test]
fn e2e_missing_directory_returns_empty() {
    let loader = SkillLoader::new("/tmp/__definitely_nonexistent_skills_dir_99x__");
    let skills = loader.load_all().unwrap();
    assert!(skills.is_empty());
}

/// Guards the Superpowers-core skills bundled at the workspace root
/// (`.deepnova/skills/`). Every bundled `.md` must parse into a valid skill.
#[test]
fn e2e_bundled_superpowers_skills_all_parse() {
    // crate dir is <workspace>/crates/deepnova-skills; skills live at <workspace>/.deepnova/skills
    let bundled = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../.deepnova/skills");
    if !bundled.exists() {
        // Bundled skills are optional in minimal checkouts; nothing to verify.
        return;
    }

    let loader = SkillLoader::new(&bundled);
    let skills = loader.load_all().unwrap();

    let names: Vec<&str> = skills.iter().map(|s| s.name.as_str()).collect();
    for expected in [
        "brainstorming",
        "systematic-debugging",
        "test-driven-development",
        "writing-plans",
        "verification-before-completion",
    ] {
        assert!(
            names.contains(&expected),
            "bundled skill '{expected}' failed to load (parsed: {names:?})"
        );
    }

    // Every parsed skill must have a non-empty prompt body.
    for s in &skills {
        assert!(
            !s.system_prompt.trim().is_empty(),
            "bundled skill '{}' has empty system prompt",
            s.name
        );
    }
}

#[tokio::test]
async fn e2e_skill_tool_registry_compatible() {
    // Verify SkillTool satisfies the Tool trait at dispatch level
    let skill = Skill {
        name: "test-skill".into(),
        description: "Test".into(),
        model: None,
        tools_allowed: vec![],
        system_prompt: "Do the thing.".into(),
    };

    let tool: Arc<dyn Tool> = Arc::new(SkillTool::new(skill));
    let ctx = ToolContext::new("call-1");

    let result = tool.execute(&ctx, "{}").await.unwrap();
    assert!(result.contains("Skill Activated: test-skill"));
    assert!(result.contains("Do the thing."));
    assert!(tool.read_only());
}

#[test]
fn e2e_builtin_skills_load() {
    let skills = deepnova_skills::load_builtin_skills();
    assert!(!skills.is_empty(), "builtin skills should load");

    let names: Vec<&str> = skills.iter().map(|s| s.name.as_str()).collect();
    for expected in [
        "frontend-developer",
        "coding-copilot",
        "loop-engineering",
        "first-principles",
        "adversarial-review",
    ] {
        assert!(
            names.contains(&expected),
            "builtin skill '{expected}' not found (got: {names:?})"
        );
    }

    // Every builtin must have a non-empty prompt
    for s in &skills {
        assert!(
            !s.system_prompt.trim().is_empty(),
            "builtin skill '{}' has empty system prompt",
            s.name
        );
        assert!(
            !s.description.trim().is_empty(),
            "builtin skill '{}' has empty description",
            s.name
        );
    }
}
