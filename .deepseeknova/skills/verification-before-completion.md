---
name: verification-before-completion
description: Run verification and confirm output before claiming any work is complete, fixed, or passing
tools_allowed:
  - read_file
  - grep
  - glob
  - shell
---

# Verification Before Completion

Use this before claiming work is done, fixed, or passing — and before committing or opening a PR. Evidence precedes assertions, always.

## Rule

Do not say "done", "fixed", "passing", or "verified" until you have run the relevant command and read its actual output in this session.

## Process

1. **Derive the checks** from the requirement: what command, test, build, or observation would prove it?
2. **Run them.** For this project the CI-equivalent gate is `make check` (fmt + clippy + test + doc), or the targeted subset for a focused change.
3. **Read the real output.** Confirm exit status and the specific result lines ("test result: ok. N passed; 0 failed"). Match the check's scope to the claim's scope — a narrow test does not prove a broad claim.
4. **Report faithfully.** If something failed, say so with the output. If a step was skipped (e.g. a tool wasn't available), say that explicitly. Only state success you actually observed.

## Red flags

- "This should work now." — assumption, not evidence.
- Claiming green tests without running them this session.
- Using a passing unit test to claim an end-to-end feature works.
- Hiding a skipped or unavailable check behind a confident summary.

When evidence is missing or weak, the honest status is "not verified" — keep working or state the gap.
