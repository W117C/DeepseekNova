import { useState, useCallback, useRef, useEffect } from "react";
import { submitPrompt, cancelRun, newSession, listSkills, getCapabilities } from "./bridge";
import type { Capabilities, SkillSummary, UsageInfo, Message, SessionSummary, AgentStatus, ApprovalRequest, ToolCall, ContextFile } from "./types";
import TitleBar from "./components/TitleBar";
import SidebarPanel from "./components/SidebarPanel";
import { ApprovalCard, ToolCallCard, ReasoningDisclosure } from "./components/ThreadCards";
import ContextPanel from "./components/ContextPanel";
import StatusBar from "./components/StatusBar";
import MarkdownRenderer from "./components/MarkdownRenderer";

function uid() { return (Date.now() + Math.random()).toString(36); }

export default function App() {
  const [messages, setMessages] = useState<Message[]>([]);
  const [input, setInput] = useState("");
  const [running, setRunning] = useState(false);
  const [capabilities, setCapabilities] = useState<Capabilities | null>(null);
  const [skills, setSkills] = useState<SkillSummary[]>([]);
  const [showSettings, setShowSettings] = useState(false);
  const [contextCollapsed, setContextCollapsed] = useState(true);
  const [lastUsage, setLastUsage] = useState<UsageInfo | null>(null);
  const [sessionCache, setSessionCache] = useState({ hit: 0, miss: 0 });
  const [reasoningEffort, setReasoningEffort] = useState("high");
  const [thinkingEnabled, setThinkingEnabled] = useState(true);
  const streamingText = useRef("");
  const streamingMsgId = useRef("");
  const streamingReasoning = useRef("");
  const streamingReasoningMsgId = useRef("");
  // Agent status for StatusBar
  const [agentStatus, setAgentStatus] = useState<AgentStatus>("ready");

  // Sample sessions + context (wired from backend later)
  const sessions: SessionSummary[] = [{ id: "1", title: "Current", active: true }];
  const [contextFiles] = useState<ContextFile[]>([]);
  const [modifiedFiles] = useState<ContextFile[]>([]);
  const [pendingApproval, setPendingApproval] = useState<ApprovalRequest | null>(null);
  const [toolCalls, setToolCalls] = useState<ToolCall[]>([]);
  const promptHistory = useRef<string[]>([]);
  const promptHistoryIdx = useRef(-1);
  const savedDraft = useRef("");

  useEffect(() => {
    getCapabilities().then((c) => setCapabilities(c as unknown as Capabilities)).catch(console.error);
    listSkills().then(setSkills).catch(console.error);
  }, []);

  const addMessage = useCallback((msg: Message) => setMessages((p) => [...p, msg]), []);
  const updateMessage = useCallback((id: string, updater: (m: Message) => Message) => setMessages((p) => p.map((m) => (m.id === id ? updater(m) : m))), []);

  const handleSubmit = useCallback(async () => {
    const prompt = input.trim();
    if (!prompt || running) return;
    setInput(""); setRunning(true); setAgentStatus("running");
    streamingText.current = ""; streamingMsgId.current = "";
    streamingReasoning.current = ""; streamingReasoningMsgId.current = "";
    addMessage({ id: uid(), role: "user", content: prompt });
    promptHistory.current.push(prompt);
    promptHistoryIdx.current = -1; savedDraft.current = "";

    const handlers = {
      onText(text: string) {
        if (streamingReasoningMsgId.current) {
          updateMessage(streamingReasoningMsgId.current, (m) => ({ ...m, reasoningDone: true }));
          streamingReasoningMsgId.current = "";
        }
        streamingText.current += text;
        if (!streamingMsgId.current) { streamingMsgId.current = uid(); addMessage({ id: streamingMsgId.current, role: "assistant", content: "" }); }
        updateMessage(streamingMsgId.current, (m) => ({ ...m, content: streamingText.current }));
      },
      onReasoning(text: string) {
        streamingReasoning.current += text;
        if (!streamingReasoningMsgId.current) {
          streamingReasoningMsgId.current = uid();
          addMessage({ id: streamingReasoningMsgId.current, role: "reasoning", content: streamingReasoning.current, reasoningDone: false });
        } else {
          updateMessage(streamingReasoningMsgId.current, (m) => ({ ...m, content: streamingReasoning.current }));
        }
      },
      onToolCallStart(id: string, name: string) {
        setToolCalls((p) => [...p, { id, command: name, status: "running" }]);
        addMessage({ id, role: "tool", content: "", toolName: name, toolId: id });
      },
      onToolCallDelta(id: string, argsDelta: string) {
        updateMessage(id, (m) => ({ ...m, content: m.content + argsDelta, toolArgs: (m.toolArgs ?? "") + argsDelta }));
        setToolCalls((p) => p.map((tc) => tc.id === id ? { ...tc, detail: (tc.detail ?? "") + argsDelta } : tc));
      },
      onToolCallEnd(id: string, name: string, args_: string) {
        updateMessage(id, (m) => ({ ...m, toolName: name, content: args_, toolArgs: args_ }));
        setToolCalls((p) => p.map((tc) => tc.id === id ? { ...tc, status: "success", detail: args_ } : tc));
      },
      onToolResult(callId: string, result: string) {
        updateMessage(callId, (m) => ({ ...m, toolResult: result }));
        setToolCalls((p) => p.map((tc) => tc.id === callId ? { ...tc, detail: (tc.detail ?? "") + "\n\nResult: " + result } : tc));
      },
      onUsage(usage: UsageInfo) {
        setLastUsage(usage);
        setSessionCache((p) => ({ hit: p.hit + usage.cache_hit_tokens, miss: p.miss + usage.cache_miss_tokens }));
      },
      onDone(text: string) {
        if (streamingReasoningMsgId.current) { updateMessage(streamingReasoningMsgId.current, (m) => ({ ...m, reasoningDone: true })); streamingReasoningMsgId.current = ""; }
        if (text && streamingMsgId.current) { updateMessage(streamingMsgId.current, (m) => ({ ...m, content: text })); }
        streamingMsgId.current = ""; setRunning(false); setAgentStatus("ready");
      },
      onError(message: string) {
        addMessage({ id: uid(), role: "assistant", content: `Error: ${message}` });
        setRunning(false); setAgentStatus("ready");
      },
    };

    try {
      await submitPrompt({ prompt, reasoning_effort: reasoningEffort, thinking_enabled: thinkingEnabled }, handlers);
    } catch (err) {
      addMessage({ id: uid(), role: "assistant", content: `Error: ${err}` });
      setRunning(false); setAgentStatus("ready");
    }
  }, [input, running, addMessage, updateMessage, reasoningEffort, thinkingEnabled]);

  const handleCancel = useCallback(async () => { await cancelRun(); setRunning(false); setAgentStatus("ready"); }, []);

  const handleNewSession = useCallback(async () => {
    if (running) return;
    await newSession();
    setMessages([]); setLastUsage(null); setSessionCache({ hit: 0, miss: 0 });
    setToolCalls([]); setPendingApproval(null);
    streamingText.current = ""; streamingMsgId.current = "";
    streamingReasoning.current = ""; streamingReasoningMsgId.current = "";
  }, [running]);

  const cachePct = sessionCache.hit + sessionCache.miss > 0 ? Math.round(sessionCache.hit / (sessionCache.hit + sessionCache.miss) * 100) : 0;

  return (
    <div className="app-shell">
      <TitleBar
        title="DPronix"
        thinkingLabel={running ? "thinking" : undefined}
        effort={reasoningEffort}
        onToggleContext={() => setContextCollapsed((v) => !v)}
        onOpenSettings={() => setShowSettings((v) => !v)}
      />

      <div className={`body${contextCollapsed ? " context-collapsed" : ""}`}>
        <SidebarPanel
          sessions={sessions}
          skillsSlot={skills.length > 0 ? skills.map((s) => <button key={s.name} className="list-item" title={s.description}>{s.name}</button>) : undefined}
        />

        <div className="main">
          <div className="thread">
            {pendingApproval && (
              <ApprovalCard
                request={pendingApproval}
                onApprove={() => setPendingApproval(null)}
                onReject={() => setPendingApproval(null)}
              />
            )}

            {toolCalls.map((tc) => (
              <ToolCallCard key={tc.id} call={tc} />
            ))}

            {messages.filter((m) => m.role === "reasoning").map((m) => (
              <ReasoningDisclosure key={m.id} durationSeconds={m.reasoningDone ? undefined : undefined}>
                {m.content}
              </ReasoningDisclosure>
            ))}

            {messages.filter((m) => m.role === "assistant" && m.content).map((m) => (
              <div key={m.id}><MarkdownRenderer content={m.content} /></div>
            ))}

            {messages.filter((m) => m.role === "user").map((m) => (
              <div key={m.id} style={{ fontSize: 13, lineHeight: 1.72, color: "var(--rx-text-primary)", whiteSpace: "pre-wrap", padding: "8px 12px", background: "var(--rx-surface-1)", borderRadius: "var(--rx-radius-lg)", alignSelf: "flex-end", maxWidth: "82%" }}>{m.content}</div>
            ))}

            {running && messages.length === 0 && (
              <div style={{ flex: 1, display: "flex", alignItems: "center", justifyContent: "center", color: "var(--rx-text-muted)", fontSize: 13 }}>DPronix Desktop - DeepSeek-V4 AI agent</div>
            )}

            {showSettings && (
              <div className="plan-card"><p className="title">Settings</p>
                <div style={{ fontSize: 12, color: "var(--rx-text-secondary)", display: "flex", flexDirection: "column", gap: 6 }}>
                  <label className="list-item" style={{ display: "flex", alignItems: "center", gap: 8, cursor: "pointer" }}>
                    <input type="checkbox" checked={thinkingEnabled} onChange={() => setThinkingEnabled((v: boolean) => !v)} /> Thinking mode
                  </label>
                  <div style={{ display: "flex", alignItems: "center", gap: 8 }}>
                    <span>Effort:</span>
                    <select value={reasoningEffort} onChange={(e) => setReasoningEffort(e.target.value)} className="send" style={{ background: "var(--rx-surface-2)", color: "var(--rx-text-primary)", border: "1px solid var(--rx-border)", fontSize: 12, padding: "2px 6px", borderRadius: "var(--rx-radius)", height: "auto" }}>
                      {(capabilities?.reasoning_effort_levels ?? ["low","medium","high"]).map((l: string) => <option key={l} value={l}>{l}</option>)}
                    </select>
                  </div>
                  <button className="btn" onClick={handleNewSession}>New Session</button>
                </div>
              </div>
            )}
          </div>

          <div className="composer">
            <textarea
              value={input}
              onChange={(e) => setInput(e.target.value)}
              rows={1}
              placeholder="Continue conversation..."
              disabled={running}
              onKeyDown={(e) => {
                const mod = e.metaKey || e.ctrlKey;
                // Prompt history: Cmd+Up / Cmd+Down
                if (mod && e.key === "ArrowUp") {
                  e.preventDefault();
                  if (promptHistory.current.length > 0 && promptHistoryIdx.current < promptHistory.current.length - 1) {
                    if (promptHistoryIdx.current === -1) savedDraft.current = input;
                    promptHistoryIdx.current++;
                    setInput(promptHistory.current[promptHistory.current.length - 1 - promptHistoryIdx.current]);
                  }
                  return;
                }
                if (mod && e.key === "ArrowDown") {
                  e.preventDefault();
                  if (promptHistoryIdx.current > 0) {
                    promptHistoryIdx.current--;
                    setInput(promptHistory.current[promptHistory.current.length - 1 - promptHistoryIdx.current]);
                  } else if (promptHistoryIdx.current === 0) {
                    promptHistoryIdx.current = -1;
                    setInput(savedDraft.current);
                  }
                  return;
                }
                // Cmd+L: focus stays (default textarea behavior)
                if (e.key === "Enter" && !e.shiftKey) { e.preventDefault(); running ? handleCancel() : handleSubmit(); }
              }}
            />
            <button className="send" aria-label="Send" disabled={!input.trim() && !running} onClick={running ? handleCancel : handleSubmit}>
              {running ? "\u25A0" : "\u2192"}
            </button>
          </div>
        </div>

        <ContextPanel
          files={contextFiles}
          modified={modifiedFiles}
          memoryCount={3}
          collapsed={contextCollapsed}
        />
      </div>

      <StatusBar
        tokensUp={lastUsage?.prompt_tokens ?? 0}
        tokensDown={lastUsage?.completion_tokens ?? 0}
        cachePercent={cachePct}
        status={agentStatus}
      />
    </div>
  );
}
