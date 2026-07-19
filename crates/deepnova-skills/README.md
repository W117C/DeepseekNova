# Skill system for deepnova

Skills are reusable prompt templates stored as markdown files with YAML
frontmatter in `.deepnova/skills/`. Each skill is exposed as a tool so
the agent can activate it during a conversation.

## Quick start

```rust,no_run
use deepnova_skills::{SkillLoader, SkillTool};
use std::sync::Arc;

// Load skills from the project's .deepnova/skills/ directory
let loader = SkillLoader::new(".deepnova/skills");
let skills = loader.load_all().unwrap();

// Wrap each skill as a Tool for the registry
let tools: Vec<Arc<dyn deepnova_core::Tool>> = skills
    .into_iter()
    .map(|s| Arc::new(SkillTool::new(s)) as Arc<dyn deepnova_core::Tool>)
    .collect();
```

## License

Licensed under the same terms as deepnova.
