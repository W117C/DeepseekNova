import { useState, useCallback, useRef, useEffect } from "react";
import { submitPrompt, cancelRun, listSkills, getCapabilities } from "./bridge";
import type { Capabilities, SkillSummary, UsageInfo, Message } from "./types";
import Transcript from "./components/Transcript";
import Composer from "./components/Composer";

function uid(): string {
  return (Date.now() + Math.random()).toString(36);
}

export default function App() {
  const [messages, setMessages] = useState<Message[]>([]);
  const [input, setInput] = useState("");
  const [running, setRunning] = useState(false);
  const [capabilities, setCapabilities] = useState<Capabilities | null>(null);
  const [skills, setSkills] = useState<SkillSummary[]>([]);
  const [showSkills, setShowSkills] = useState(false);
  const [lastUsage, setLastUsage] = useState<UsageInfo | null>(null);
  const [sessionCache, setSessionCache] = useState({ hit: 0, miss: 0 });
  const [reasoningEffort, setReasoningEffort] = useState("high");
  const [thinkingEnabled, setThinkingEnabled] = useState(true);
  const streamingText = useRef("");
  const streamingMsgId = useRef("");
  const streamingReasoning = useRef("");
  const streamingReasoningMsgId = useRef("");

  // Load capabilities + skills on mount
  useEffect(() => {
    getCapabilities().then((c) => setCapabilities(c as unknown as Capabilities)).catch(console.error);
    listSkills().then(setSkills).catch(console.error);
  }, []);

  const addMessage = useCallback((msg: Message) => {
    setMessages((prev) => [...prev, msg]);
  }, []);

  const updateMessage = useCallback((id: string, updater: (m: Message) => Message) => {
    setMessages((prev) => prev.map((m) => (m.id === id ? updater(m) : m)));
  }, []);

  const handleSubmit = useCallback(async () => {
    const prompt = input.trim();
    if (!prompt || running) return;
    setInput("");
    setRunning(true);
    streamingText.current = "";
    streamingMsgId.current = "";
    streamingReasoning.current = "";
    streamingReasoningMsgId.current = "";

    addMessage({ id: uid(), role: "user", content: prompt });

    const handlers = {
      onText(text: string) {
        // Thinking chain (if any) is complete once visible output starts.
        if (streamingReasoningMsgId.current) {
          updateMessage(streamingReasoningMsgId.current, (m) => ({ ...m, reasoningDone: true }));
          streamingReasoningMsgId.current = "";
        }
        streamingText.current += text;
        if (!streamingMsgId.current) {
          streamingMsgId.current = uid();
          addMessage({ id: streamingMsgId.current, role: "assistant", content: "" });
        }
        updateMessage(streamingMsgId.current, (m) => ({ ...m, content: streamingText.current }));
      },
      onReasoning(text: string) {
        // Accumulate DeepSeek-V4 thinking deltas into a single collapsible card
        // instead of one card per delta.
        streamingReasoning.current += text;
        if (!streamingReasoningMsgId.current) {
          streamingReasoningMsgId.current = uid();
          addMessage({
            id: streamingReasoningMsgId.current,
            role: "reasoning",
            content: streamingReasoning.current,
            reasoningDone: false,
          });
        } else {
          updateMessage(streamingReasoningMsgId.current, (m) => ({
            ...m,
            content: streamingReasoning.current,
          }));
        }
      },
      onToolCallStart(id: string, name: string) {
        addMessage({ id, role: "tool", content: "", toolName: name, toolId: id });
      },
      onToolCallDelta(id: string, argsDelta: string) {
        updateMessage(id, (m) => ({
          ...m, content: m.content + argsDelta, toolArgs: (m.toolArgs ?? "") + argsDelta,
        }));
      },
      onToolCallEnd(id: string, name: string, arguments_: string) {
        updateMessage(id, (m) => ({ ...m, toolName: name, content: arguments_, toolArgs: arguments_ }));
      },
      onToolResult(callId: string, result: string) {
        updateMessage(callId, (m) => ({ ...m, toolResult: result }));
      },
      onUsage(usage: UsageInfo) {
        setLastUsage(usage);
        setSessionCache(prev => ({ hit: prev.hit + usage.cache_hit_tokens, miss: prev.miss + usage.cache_miss_tokens }));
      },
      onDone(text: string) {
        if (streamingReasoningMsgId.current) {
          updateMessage(streamingReasoningMsgId.current, (m) => ({ ...m, reasoningDone: true }));
          streamingReasoningMsgId.current = "";
        }
        if (text && streamingMsgId.current) {
          updateMessage(streamingMsgId.current, (m) => ({ ...m, content: text }));
        }
        streamingMsgId.current = "";
        setRunning(false);
      },
      onError(message: string) {
        addMessage({ id: uid(), role: "assistant", content: `Error: ${message}` });
        setRunning(false);
      },
    };

    try {
      await submitPrompt({ prompt, reasoning_effort: reasoningEffort, thinking_enabled: thinkingEnabled }, handlers);
    } catch (err) {
      addMessage({ id: uid(), role: "assistant", content: `Error: ${err}` });
      setRunning(false);
    }
  }, [input, running, addMessage, updateMessage, reasoningEffort, thinkingEnabled]);

  const handleCancel = useCallback(async () => { await cancelRun(); setRunning(false); }, []);

  return (
    <div className="app-container">
      {/* Header */}
      <header className="app-header">
        <div className="header-left">
          <h1 className="header-logo">DPronix</h1>
          <span className="header-badge">desktop v{capabilities?.version ?? "dev"}</span>
        </div>
        <div className="header-center">
          {capabilities?.supports_thinking && (
            <label className="chip chip-toggle" title="DeepSeek-V4 thinking mode">
              <input
                type="checkbox"
                checked={thinkingEnabled}
                disabled={running}
                onChange={(e) => setThinkingEnabled(e.target.checked)}
              />
              thinking
            </label>
          )}
          {capabilities?.supports_reasoning_effort && (
            <label className="chip" title="Reasoning effort passed to the model">
              effort:
              <select
                value={reasoningEffort}
                disabled={running || !thinkingEnabled}
                onChange={(e) => setReasoningEffort(e.target.value)}
              >
                {(capabilities.reasoning_effort_levels ?? ["low", "medium", "high"]).map((lvl) => (
                  <option key={lvl} value={lvl}>{lvl}</option>
                ))}
              </select>
            </label>
          )}
        </div>
        <div className="header-right">
          <button className={`btn-icon ${showSkills ? "active" : ""}`} onClick={() => setShowSkills(!showSkills)} title="Skills">
            <svg width="18" height="18" viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth="2">
              <path d="M12 2L2 7l10 5 10-5-10-5z"/><path d="M2 17l10 5 10-5"/><path d="M2 12l10 5 10-5"/>
            </svg>
          </button>
        </div>
      </header>

      {/* Skills panel */}
      {showSkills && (
        <div className="skills-panel">
          <div className="skills-panel-header">
            <h3>Skills</h3>
            <button className="btn-icon-small" onClick={() => setShowSkills(false)}>✕</button>
          </div>
          {skills.length === 0 && <p className="muted">No skills found. Create .md files in .dpronix/skills/</p>}
          {skills.map((s) => (
            <div key={s.name} className="skill-card">
              <strong>{s.name}</strong> — {s.description}
              {s.tools_allowed.length > 0 && (
                <div className="skill-tags">{s.tools_allowed.map((t) => <span key={t} className="tag">{t}</span>)}</div>
              )}
            </div>
          ))}
        </div>
      )}

      {/* Transcript */}
      <Transcript messages={messages} loading={running && messages.length > 0 && !streamingMsgId.current} />

      {/* Status bar */}
      <div className="status-bar">
        {lastUsage && (
          <span className="status-item" title="Token usage for last turn">
            {lastUsage.prompt_tokens}↑ {lastUsage.completion_tokens}↓
            {lastUsage.reasoning_tokens > 0 && (
              <span className="status-item" title="DeepSeek-V4 billed reasoning tokens">
                🧠 {lastUsage.reasoning_tokens}
              </span>
            )}
            {sessionCache.hit + sessionCache.miss > 0 && (
              <span className="status-item" title="Session cache hit rate">
                💡 cache {Math.round(sessionCache.hit / (sessionCache.hit + sessionCache.miss) * 100)}%
              </span>
            )}
          </span>
        )}
        <span className="status-item status-right">{running ? "running…" : "ready"}</span>
      </div>

      {/* Composer */}
      <Composer
        value={input}
        onChange={setInput}
        onSubmit={handleSubmit}
        onCancel={handleCancel}
        running={running}
      />
    </div>
  );
}
