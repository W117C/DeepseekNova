---
name: writing-plans
description: Turn a spec or requirements into a concrete, decision-complete, multi-step implementation plan
tools_allowed:
  - read_file
  - grep
  - glob
---

# Writing Plans

Use this when you have a spec or requirements for a multi-step task, before touching code. A good plan lets an implementer proceed without making further design decisions.

## What a plan must contain

1. **Summary.** One or two sentences: what this delivers and why.
2. **Key changes, grouped by subsystem or behavior.** Use descriptive headers (e.g. "Wire protocol", "Frontend status bar", "Provider factory") rather than "Step 1/2/3". Each group is a few high-signal bullets.
3. **Specific file paths** only where they disambiguate non-obvious work. Don't list every file.
4. **Test plan.** What proves each change works — commands, cases, gates.
5. **Assumptions.** Anything the plan depends on that could be wrong.

## Principles

- **Decision-complete:** no "figure out later" gaps in the critical path.
- **Glanceable:** compress related changes; omit branch-by-branch detail unless it prevents a mistake.
- **Scoped to the real end state**, not the easiest subset that would pass a test.
- Resolve open questions with the user *before* finalizing — don't bury them in the plan.

## Red flags

- Vague steps ("update the backend") with no concrete change described.
- No test plan, so "done" is undefined.
- Plan silently narrows the objective to something easier.
