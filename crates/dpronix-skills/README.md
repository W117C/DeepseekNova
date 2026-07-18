# Skill system for dpronix

Skills are reusable prompt templates stored as markdown files with YAML
frontmatter in `.dpronix/skills/`. Each skill is exposed as a tool so
the agent can activate it during a conversation.

## Quick start

```rust,no_run
use dpronix_skills::{SkillLoader, SkillTool};
use std::sync::Arc;

// Load skills from the project's .dpronix/skills/ directory
let loader = SkillLoader::new(".dpronix/skills");
let skills = loader.load_all().unwrap();

// Wrap each skill as a Tool for the registry
let tools: Vec<Arc<dyn dpronix_core::Tool>> = skills
    .into_iter()
    .map(|s| Arc::new(SkillTool::new(s)) as Arc<dyn dpronix_core::Tool>)
    .collect();
```

## License

Licensed under the same terms as dpronix.
