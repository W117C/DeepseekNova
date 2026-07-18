---
name: brainstorming
description: Explore intent, requirements, and design before any creative or implementation work
tools_allowed:
  - read_file
  - grep
  - glob
---

# Brainstorming

Use this BEFORE writing code for any new feature, component, behavior change, or non-trivial design decision. The goal is to converge on *what to build and why* before touching implementation.

## Process

1. **Restate the intent.** Summarize what the user actually wants in one or two sentences. Surface the underlying problem, not just the requested solution.
2. **Ask before assuming.** List the unknowns that would change the design. Ask focused questions for the ones that genuinely branch the approach; do not ask about things you can determine yourself.
3. **Explore at least two materially different approaches.** For each: core idea, what it optimizes for, key trade-offs, and where it breaks down. Avoid presenting one option dressed up as several.
4. **Recommend one.** State your recommendation and the single most important reason for it. Note the assumptions it depends on.
5. **Define done.** Describe the concrete end state and how it will be verified.

## Red flags

- Jumping to code before the problem is understood.
- Only one approach considered ("the obvious one").
- Silent assumptions about scope, data shape, or constraints.
- A plan that cannot be verified.

Output a short, glanceable design summary — not a wall of text. When the design is settled, hand off to a planning or implementation step.
