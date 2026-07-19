---
name: loop-engineering
description: Iterative improvement loop — generate, evaluate, and refine output until quality criteria are met. Use for any task that requires multiple rounds of refinement.
model: null
tools_allowed:
  - write_file
  - read_file
  - edit_file
  - shell
  - grep
  - glob
  - web_fetch
---

# Loop Engineering Skill

You operate in an iterative refinement loop. Each iteration improves the output based on measurable quality criteria.

## Loop Structure

```
┌─────────────────────────────────┐
│  1. GENERATE (create/modify)    │
│  2. EVALUATE (test/check/review) │
│  3. ANALYZE (identify gaps)      │
│  4. IMPROVE (fix issues)         │
│  5. VERIFY (re-test)             │
│           ↻ repeat                │
└─────────────────────────────────┘
```

## Execution Rules

1. **Define success criteria upfront**: What does "done" look like? Write it down.
2. **Maximum 5 iterations**: Don't loop forever. If not converged after 5, report and ask.
3. **Measure improvement**: Each iteration must show measurable progress. If iteration N+1 doesn't improve on N, stop.
4. **Track changes**: Log what changed in each iteration and why.

## Iteration Protocol

### Iteration 1: Draft
- Generate the initial output
- Run tests/linters/validators
- Record baseline quality score

### Iteration 2: Critical Review
- Identify weaknesses (correctness, performance, readability, edge cases)
- Fix critical issues first (correctness > security > performance > style)

### Iteration 3: Polish
- Address style, naming, documentation
- Refactor for clarity
- Ensure tests cover edge cases

### Iteration 4: Hardening
- Add error handling
- Consider concurrency safety
- Performance check (premature optimization is OK to skip)

### Iteration 5: Final Verification
- Full test suite
- Clippy/linter clean
- Documentation complete
- No TODOs left

## Stop Conditions

- ✅ All success criteria met → stop, report success
- ⚠️ 5 iterations reached → stop, report current state + remaining issues
- ❌ Quality regression (score decreased) → stop, revert to previous, report

## Quality Scoring

Score each dimension 0-3:
- **Correctness**: Does it work? (0=broken, 1=partial, 2=works, 3=perfect)
- **Test coverage**: (0=none, 1=smoke, 2=main paths, 3=comprehensive)
- **Code quality**: (0=messy, 1=readable, 2=clean, 3=exemplary)
- **Documentation**: (0=none, 1=minimal, 2=good, 3=complete)

Threshold: total ≥ 9/12 and no dimension = 0 → ready to ship.
