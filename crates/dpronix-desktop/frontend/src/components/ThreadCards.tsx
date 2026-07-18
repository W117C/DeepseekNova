/**
 * ThreadCards — Reasonix UI: ApprovalCard, ToolCallCard, ReasoningDisclosure, PlanStepsCard
 */
import { useState } from "react";
import type { ApprovalRequest, ToolCall, PlanStep } from "../types";

export function ApprovalCard({ request, onApprove, onReject }: { request: ApprovalRequest; onApprove: (id: string) => void; onReject: (id: string) => void }) {
  return (
    <div className="approval-card" role="alert">
      <p className="label">Needs Confirmation</p>
      <p className="title">{request.title}</p>
      {request.description && <p className="description">{request.description}</p>}
      <div className="actions">
        <button className="btn" onClick={() => onReject(request.id)}>Reject</button>
        <button className="btn btn-approve" onClick={() => onApprove(request.id)}>Approve</button>
      </div>
    </div>
  );
}

export function ToolCallCard({ call, defaultOpen }: { call: ToolCall; defaultOpen?: boolean }) {
  const [open, setOpen] = useState(defaultOpen ?? false);
  const statusIcon = call.status === "running" ? "..." : call.status === "success" ? "\u2713" : "\u2715";
  return (
    <div className="tool-card">
      <button className="summary" aria-expanded={open} onClick={() => setOpen(!open)}>
        <span className={`chevron${open ? " open" : ""}`}>&rsaquo;</span>
        <span className="command">{call.command}</span>
        <span className={`status ${call.status}`}>{statusIcon}</span>
      </button>
      {open && call.detail && <pre className="detail">{call.detail}</pre>}
    </div>
  );
}

export function ReasoningDisclosure({ durationSeconds, children }: { durationSeconds?: number; children: React.ReactNode }) {
  const [open, setOpen] = useState(false);
  return (
    <div className="reasoning">
      <button className="summary" aria-expanded={open} onClick={() => setOpen(!open)}>
        <span className={`chevron${open ? " open" : ""}`}>&rsaquo;</span>
        <span>Reasoning{durationSeconds ? ` (${durationSeconds}s)` : ""}</span>
      </button>
      {open && <div className="content">{children}</div>}
    </div>
  );
}

export function PlanStepsCard({ title, steps }: { title: string; steps: PlanStep[] }) {
  return (
    <div className="plan-card">
      <p className="title">{title}</p>
      <div className="steps">
        {steps.map((s) => (
          <div key={s.id} className="step">
            <span className={`icon ${s.status}`}>
              {s.status === "done" ? "\u2713" : s.status === "active" ? "\u25CF" : "\u25CB"}
            </span>
            <span className={`label ${s.status}`}>{s.title}</span>
          </div>
        ))}
      </div>
    </div>
  );
}
