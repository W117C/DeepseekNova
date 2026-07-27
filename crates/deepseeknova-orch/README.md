# deepseeknova-orch

Multi-agent orchestration for DeepSeek-V4: Goal-Oriented Action Planning (GOAP),
swarm coordination, and agent federation. Inspired by Ruflo's goal planner
and swarm system, optimized for DeepSeek-V4's thinking mode and context caching.

> **Status: experimental.** Not yet wired into any frontend (CLI/desktop/serve) —
> no external callers; exercised only by this crate's own tests.

## Architecture

```text
User Goal
   │
   ▼
GoalPlanner (GOAP: dependency/topological scheduling)
   │  └─ decomposes goal → Action DAG
   ▼
SwarmCoordinator (Queen-led)
   ├─ Worker Agent 1 (sub-goal A)
   ├─ Worker Agent 2 (sub-goal B)
   ├─ Worker Agent 3 (sub-goal C)
   └─ Shared Memory (AgentDB / HNSW)
   │
   ▼
Execution → Results → Learning Loop → Memory
```
