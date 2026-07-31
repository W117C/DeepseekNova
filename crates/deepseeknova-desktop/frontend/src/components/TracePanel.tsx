/**
 * TracePanel.tsx — Trajectory Trace 可视化面板
 *
 * 将 Agent 执行轨迹以垂直时间线形式呈现：
 * thinking / tool_call / tool_result / text / error / done
 */

import { useState, useMemo } from "react";
import { useStore } from "../store";
import { useI18n } from "../i18n";
import type { WireEvent } from "../types";

interface TraceStep {
  type: "thinking" | "tool_call" | "tool_result" | "verification" | "text" | "error" | "done";
  label: string;
  detail?: string;
  mono?: string;
  failed?: boolean;
  ts: number;
}

/** Convert raw WireEvent stream into displayable trace steps. */
function buildTraceSteps(
  events: { event: WireEvent; ts: number }[]
): TraceStep[] {
  const steps: TraceStep[] = [];
  let textBuffer = "";
  let textTs = 0;
  let reasoningBuffer = "";
  let reasoningTs = 0;

  const flushText = () => {
    if (textBuffer) {
      steps.push({
        type: "text",
        label: `文本生成 (${textBuffer.length} chars)`,
        detail: textBuffer.length > 200 ? textBuffer.slice(0, 200) + "…" : textBuffer,
        ts: textTs,
      });
      textBuffer = "";
    }
  };

  const flushReasoning = () => {
    if (reasoningBuffer) {
      steps.push({
        type: "thinking",
        label: `推理 (${reasoningBuffer.length} chars)`,
        detail: reasoningBuffer.length > 300 ? reasoningBuffer.slice(0, 300) + "…" : reasoningBuffer,
        ts: reasoningTs,
      });
      reasoningBuffer = "";
    }
  };

  for (const { event, ts } of events) {
    switch (event.kind) {
      case "text_delta":
        if (!textBuffer) textTs = ts;
        textBuffer += event.text;
        break;

      case "reasoning_delta":
        flushText();
        if (!reasoningBuffer) reasoningTs = ts;
        reasoningBuffer += event.text;
        break;

      case "tool_call_start":
        flushText();
        flushReasoning();
        steps.push({
          type: "tool_call",
          label: event.name,
          mono: event.name,
          ts,
        });
        break;

      case "tool_call_end":
        steps.push({
          type: "tool_call",
          label: `${event.name} 调用完成`,
          mono: event.name,
          detail: event.arguments.length > 200 ? event.arguments.slice(0, 200) + "…" : event.arguments,
          ts,
        });
        break;

      case "tool_result":
        steps.push({
          type: "tool_result",
          label: "工具返回",
          detail: event.result.length > 300 ? event.result.slice(0, 300) + "…" : event.result,
          ts,
        });
        break;

      case "verification":
        flushText();
        flushReasoning();
        steps.push({
          type: "verification",
          label: event.passed ? `✓ ${event.command}` : `✗ ${event.command}`,
          failed: !event.passed,
          detail: event.passed
            ? undefined
            : event.summary.length > 300
              ? event.summary.slice(0, 300) + "…"
              : event.summary,
          ts,
        });
        break;

      case "error":
        flushText();
        flushReasoning();
        steps.push({
          type: "error",
          label: "错误",
          detail: event.message,
          ts,
        });
        break;

      case "done":
        flushText();
        flushReasoning();
        steps.push({
          type: "done",
          label: event.usage
            ? `完成 — ${event.usage.total_tokens} tokens`
            : "完成",
          ts,
        });
        break;

      case "usage":
        // Usage events are summarized in the done step
        break;

      default:
        break;
    }
  }

  flushText();
  flushReasoning();
  return steps;
}

function formatDelta(ts: number, prevTs: number | null): string {
  if (prevTs === null) return "0s";
  const ms = ts - prevTs;
  if (ms < 1000) return `+${ms}ms`;
  return `+${(ms / 1000).toFixed(1)}s`;
}

export default function TracePanel() {
  const { t } = useI18n();
  const traceEvents = useStore((s) => s.traceEvents);
  const traceStartTime = useStore((s) => s.traceStartTime);
  const [expanded, setExpanded] = useState<Set<number>>(new Set());

  const steps = useMemo(() => buildTraceSteps(traceEvents), [traceEvents]);

  const toolCallCount = steps.filter((s) => s.type === "tool_call").length;
  const totalDuration = traceStartTime && steps.length > 0
    ? ((steps[steps.length - 1].ts - traceStartTime) / 1000).toFixed(1)
    : "0";

  const toggleExpand = (idx: number) => {
    setExpanded((prev) => {
      const next = new Set(prev);
      if (next.has(idx)) next.delete(idx);
      else next.add(idx);
      return next;
    });
  };

  if (steps.length === 0) {
    return (
      <div className="trace-panel">
        <div className="empty-state">
          <div className="empty-state-icon">◎</div>
          <div className="empty-state-text">{t("panel.emptyTrace")}</div>
        </div>
      </div>
    );
  }

  return (
    <div className="trace-panel">
      {/* Summary bar */}
      <div className="trace-summary">
        <span className="trace-summary-item">
          {t("trace.steps")} <span className="trace-summary-value">{steps.length}</span>
        </span>
        <span className="trace-summary-item">
          {t("trace.tools")} <span className="trace-summary-value">{toolCallCount}</span>
        </span>
        <span className="trace-summary-item">
          {t("trace.duration")} <span className="trace-summary-value">{totalDuration}s</span>
        </span>
      </div>

      {/* Timeline */}
      <div className="trace-timeline">
        {steps.map((step, idx) => {
          const prevTs = idx > 0 ? steps[idx - 1].ts : null;
          const isExpanded = expanded.has(idx);
          const hasDetail = !!step.detail;

          return (
            <div className="trace-node" key={idx}>
              <div className={`trace-dot ${step.type}${step.failed ? " failed" : ""}`} />
              <div className="trace-content">
                <div className="trace-label">
                  {step.mono && <span className="trace-label-mono">{step.mono}</span>}
                  {!step.mono && <span>{step.label}</span>}
                  <span className="trace-time">{formatDelta(step.ts, prevTs)}</span>
                </div>
                {hasDetail && (
                  <>
                    <div className={`trace-detail ${isExpanded ? "" : "collapsed"}`}>
                      {step.detail}
                    </div>
                    <div className="trace-toggle" onClick={() => toggleExpand(idx)}>
                      {isExpanded ? t("trace.collapse") : t("trace.expand")}
                    </div>
                  </>
                )}
              </div>
            </div>
          );
        })}
      </div>
    </div>
  );
}
