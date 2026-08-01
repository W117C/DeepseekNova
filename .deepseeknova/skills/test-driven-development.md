---
name: test-driven-development
description: Write a failing test first, then implement to make it pass, for any feature or bugfix
tools_allowed:
  - read_file
  - grep
  - glob
  - shell
---

# Test-Driven Development

Use this when implementing a feature or fixing a bug. Write the test before the implementation. The failing test defines "done"; the passing test proves it.

## The Red-Green-Refactor loop

1. **Red.** Write one small test that describes the desired behavior. Run it and watch it fail for the right reason. A test that passes before you write any code is testing the wrong thing.
2. **Green.** Write the minimum implementation that makes the test pass. Resist adding untested behavior.
3. **Refactor.** With the test green, clean up names, duplication, and structure. Re-run the test to confirm it still passes.

Repeat one behavior at a time.

## For bugfixes

Write a test that reproduces the bug and fails. Fix the code until it passes. That test now guards against regression forever.

## Guidelines

- One behavior per test; a clear name that states the expectation.
- Test observable behavior, not private implementation details.
- Keep tests fast and deterministic — no hidden ordering or network dependence.
- New public API in this project MUST ship with tests (project convention).

## Red flags

- Writing all the code first and "adding tests later."
- A test that never failed — you don't know it tests anything.
- Asserting on incidental details that make refactoring painful.
