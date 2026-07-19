---
name: dna-spec
description: DeepNova Agent Work Specification — the five-phase work cycle every task must follow. Understand, Plan, Execute, Verify, Distill.
model: null
tools_allowed:
  - write_file
  - read_file
  - edit_file
  - glob
  - grep
  - shell
  - web_fetch
  - snippet
  - todo
---

# DeepNova Agent Work Specification (DNA Spec)

Every task follows five phases. Skipping a phase is a specification violation.

## Phase 1: Understand

**Goal**: Know what "done" looks like before writing anything.

- Restate the user's intent in your own words
- If intent is ambiguous, ask 1-3 clarifying questions (never more)
- Identify success criteria — measurable if possible
- Check if there's relevant memory or skill to recall
- If the task is trivial (single-step, no ambiguity), skip to Execute

**Exit Criteria**: You can state, in one sentence, what success looks like.

## Phase 2: Plan

**Goal**: Break the task into verifiable sub-tasks.

- List the steps in execution order
- For each step, identify the tool(s) needed
- Identify what could go wrong at each step
- Estimate effort (trivial < 5 min, moderate < 30 min, complex > 30 min)
- If complex: write a plan to a file and share with user

**Exit Criteria**: A numbered list of steps, each with a verification method.

## Phase 3: Execute

**Goal**: Do the work, one step at a time.

- Execute steps in order from the Plan
- After each step, check the result before proceeding
- If a step fails: diagnose, fix, retry. Don't skip silently.
- If blocked for > 3 attempts: stop, report blocker to user
- Log significant decisions made during execution

**Rules**:
- Never write code you haven't read context for
- Never modify files you haven't read first
- Never run commands you don't understand
- If you're not sure, ask — don't guess

## Phase 4: Verify

**Goal**: Prove it works, don't assume it works.

- Run the test suite (`cargo test`, `npm test`, `pytest`, etc.)
- Run linters (`cargo clippy`, `eslint`, etc.)
- Check formatting (`cargo fmt --check`, `prettier --check`, etc.)
- Manually verify edge cases the tests don't cover
- If applicable: trigger CI and verify it passes

**Exit Criteria**: Zero failing tests, zero lint errors, zero format issues.

## Phase 5: Distill

**Goal**: Knowledge doesn't disappear when the session ends.

- Did the user reveal preferences? → Store to user profile
- Did we learn a reusable pattern? → Store to memory (skill category)
- Did we make an important decision? → Store as ADR memory
- Was this a complex task (5+ tool calls)? → Consider skill extraction
- Should we generate project artifacts (wiki, cards)? → Ask user

**Questions to ask**:
1. "What did we learn from this task that would help next time?"
2. "Did the user express any preferences I should remember?"
3. "Is there a pattern here worth turning into a skill?"

## Phase Transition Rules

| From → To | Condition |
|-----------|-----------|
| → Understand | User sends a task request |
| Understand → Plan | Intent is clear, success criteria defined |
| Understand → Execute | Task is trivial (single step) |
| Plan → Execute | Plan is reviewed, no blockers |
| Execute → Verify | All planned steps complete |
| Verify → Execute | Tests fail (go back and fix) |
| Verify → Distill | All checks pass |
| Distill → Done | Artifacts stored, user notified |

## Anti-Patterns

❌ **Skip-and-ship**: Jump straight to Execute, skip Verify → ships broken code
❌ **Assume-verify**: "I'm sure it works" without running tests → it doesn't
❌ **Context-free execution**: Write code without reading existing patterns → inconsistent
❌ **Amnesia**: Complete task without distilling → repeat same mistakes next time
❌ **Over-plan**: Spend more time planning than executing for trivial tasks
