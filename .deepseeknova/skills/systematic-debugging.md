---
name: systematic-debugging
description: Reproduce and root-cause bugs, test failures, or unexpected behavior before proposing any fix
tools_allowed:
  - read_file
  - grep
  - glob
  - shell
---

# Systematic Debugging

Use this the moment you hit a bug, test failure, or behavior you did not expect. Do NOT propose a fix until you can explain the cause with evidence.

## Process

1. **Reproduce.** Find the smallest reliable way to trigger the problem. If you cannot reproduce it, that is the first thing to solve. Capture exact commands, inputs, and output.
2. **Observe, don't guess.** Read the actual error, stack trace, and surrounding code. Add logging or run targeted commands to see real state rather than assuming it.
3. **Form a hypothesis.** State a specific, falsifiable claim about the cause ("X is null because Y runs before Z").
4. **Test the hypothesis.** Make the smallest experiment that would confirm or refute it. Let the evidence decide.
5. **Fix the root cause.** Once proven, fix the actual cause — not the symptom. Avoid band-aids that hide the failure.
6. **Verify.** Re-run the reproduction and the relevant test suite. Confirm the fix works and introduces no regression.

## Red flags

- "Let me just try changing this and see." Random edits are not debugging.
- Fixing the symptom (swallowing an error, adding a null check) without knowing why it occurred.
- Claiming a fix works without re-running the failing case.
- Multiple simultaneous changes so you can't tell which one mattered.

Confidence in a fix comes from a reproduced failure that now passes — not from a plausible story.
