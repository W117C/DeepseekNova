import { useState, useCallback, useRef, useEffect } from "react";
import { submitPrompt, cancelRun, newSession, listSkills, getCapabilities } from "./bridge";
import type { Capabilities, SkillSummary, UsageInfo, Message } from "./types";
import TitleBar from "./components/TitleBar";
import Sidebar from "./components/Sidebar";
import Transcript from "./components/Transcript";
import Composer from "./components/Composer";
import StatusBar from "./components/StatusBar";
import SettingsPanel from "./components/SettingsPanel";

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
  const [showSettings, setShowSettings] = useState(false);
  const [lastUsage, setLastUsage] = useState<UsageInfo | null>(null);
  const [sessionCache, setSessionCache] = useState({ hit: 0, miss: 0 });
  const [reasoningEffort, setReasoningEffort] = useState("high");
  const [thinkingEnabled, setThinkingEnabled] = useState(true);
  const [sideCollapsed, setSideCollapsed] = useState(() => {
    return localStorage.getItem("dpronix.sideCollapsed") === "true";
  });
  const streamingText = useRef("");
  const streamingMsgId = useRef("");
  const streamingReasoning = useRef("");
  const streamingReasoningMsgId = useRef("");

  // Load capabilities + skills on mount
  useEffect(() => {
    getCapabilities().then((c) => setCapabilities(c as unknown as Capabilities)).catch(console.error);
    listSkills().then(setSkills).catch(console.error);
  }, []);

  // Persist sidebar collapse state
  useEffect(() => {
    localStorage.setItem("dpronix.sideCollapsed", String(sideCollapsed));
  }, [sideCollapsed]);

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

  const handleNewSession = useCallback(async () => {
    if (running) return;
    await newSession();
    setMessages([]);
    setLastUsage(null);
    setSessionCache({ hit: 0, miss: 0 });
    streamingText.current = "";
    streamingMsgId.current = "";
    streamingReasoning.current = "";
    streamingReasoningMsgId.current = "";
  }, [running]);

  const handleToggleSide = useCallback(() => setSideCollapsed((v) => !v), []);

  // Keyboard shortcuts
  useEffect(() => {
    const onKey = (e: KeyboardEvent) => {
      const mod = e.ctrlKey || e.metaKey;
      if (mod && (e.key === "b" || e.key === "B")) {
        e.preventDefault();
        handleToggleSide();
      } else if (mod && (e.key === "n" || e.key === "N")) {
        e.preventDefault();
        handleNewSession();
      } else if (mod && e.key === ",") {
        e.preventDefault();
        setShowSettings((v) => !v);
      } else if (e.key === "Escape" && running) {
        e.preventDefault();
        handleCancel();
      }
    };
    window.addEventListener("keydown", onKey);
    return () => window.removeEventListener("keydown", onKey);
  }, [handleToggleSide, handleNewSession, handleCancel, running]);

  const modelName = capabilities?.version
    ? `deepseek-v4`  // TODO: surface real model from backend
    : undefined;

  return (
    <div className="app" data-side-collapsed={sideCollapsed}>
      {/* Title bar */}
      <TitleBar
        sideCollapsed={sideCollapsed}
        onToggleSide={handleToggleSide}
        modelName={modelName}
        showSettings={showSettings}
        onToggleSettings={() => setShowSettings((v) => !v)}
        showSkills={showSkills}
        onToggleSkills={() => setShowSkills((v) => !v)}
        onNewSession={handleNewSession}
      />

      {/* Sidebar */}
      <Sidebar collapsed={sideCollapsed} messageCount={messages.length} />

      {/* Main area: thread + controls + composer */}
      <div className="main-area">
        {/* Skills panel (overlay) */}
        {showSkills && (
          <div className="skills-panel">
            <div className="skills-panel-header">
              <h3>Skills</h3>
              <button className="btn-icon-small" onClick={() => setShowSkills(false)}>x</button>
            </div>
            {skills.length === 0 && <p className="muted">No skills found. Create .md files in .dpronix/skills/</p>}
            {skills.map((s) => (
              <div key={s.name} className="skill-card">
                <strong>{s.name}</strong> - {s.description}
                {s.tools_allowed.length > 0 && (
                  <div className="skill-tags">{s.tools_allowed.map((t) => <span key={t} className="tag">{t}</span>)}</div>
                )}
              </div>
            ))}
          </div>
        )}

        {/* Settings panel (overlay) */}
        {showSettings && (
          <SettingsPanel capabilities={capabilities} onClose={() => setShowSettings(false)} />
        )}

        {/* Thinking / effort controls */}
        <div className="controls-row">
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
            <select
              className="field"
              value={reasoningEffort}
              disabled={running || !thinkingEnabled}
              onChange={(e) => setReasoningEffort(e.target.value)}
              title="Reasoning effort"
            >
              {(capabilities.reasoning_effort_levels ?? ["low", "medium", "high"]).map((lvl) => (
                <option key={lvl} value={lvl}>{lvl}</option>
              ))}
            </select>
          )}
        </div>

        {/* Transcript */}
        <Transcript messages={messages} loading={running && messages.length > 0 && !streamingMsgId.current} />

        {/* Composer */}
        <Composer
          value={input}
          onChange={setInput}
          onSubmit={handleSubmit}
          onCancel={handleCancel}
          running={running}
        />
      </div>

      {/* Status bar */}
      <StatusBar lastUsage={lastUsage} sessionCache={sessionCache} running={running} />
    </div>
  );
}
