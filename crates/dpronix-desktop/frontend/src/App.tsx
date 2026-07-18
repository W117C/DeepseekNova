/**
 * App — DPronix desktop root.
 *
 * Layout: TitleBar → Body(sidebar | main(thread + composer) | context) → StatusBar
 *
 * Message flow: messages are stored in arrival order and rendered through
 * <Transcript> which preserves chronological order (no filter-split).
 * Streaming state is held in refs to avoid re-render churn.
 */
import { useState, useCallback, useRef, useEffect, lazy, Suspense } from "react";
import { submitPrompt, cancelRun, newSession, listSkills, getCapabilities, respondApproval, getWorkspaceFiles, listSessions, createSession, listProviders, listMcpServers, getConfig, saveConfig, switchModel } from "./bridge";
import type {
  Capabilities,
  SkillSummary,
  UsageInfo,
  Message,
  SessionSummary,
  AgentStatus,
  ApprovalRequest,
  ContextFile,
  ProviderSummary,
  McpServer,
  AppConfig,
  Mode,
  Effort,
} from "./types";
import { DEFAULT_CONFIG } from "./types";
import TitleBar from "./components/TitleBar";
import SidebarPanel from "./components/SidebarPanel";
import Transcript from "./components/Transcript";
import ContextPanel from "./components/ContextPanel";
import StatusBar from "./components/StatusBar";
import Composer from "./components/Composer";
import SettingsPanel from "./components/SettingsPanel";
const ModeBar = lazy(() => import("./components/ModeBar"));

function uid() {
  return (Date.now() + Math.random()).toString(36);
}

export default function App() {
  const [messages, setMessages] = useState<Message[]>([]);
  const [input, setInput] = useState("");
  const [running, setRunning] = useState(false);
  const [capabilities, setCapabilities] = useState<Capabilities | null>(null);
  const [skills, setSkills] = useState<SkillSummary[]>([]);
  const [showSettings, setShowSettings] = useState(false);
  const [contextCollapsed, setContextCollapsed] = useState(true);
  const [sideCollapsed, setSideCollapsed] = useState(false);
  const [lastUsage, setLastUsage] = useState<UsageInfo | null>(null);
  const [sessionCache, setSessionCache] = useState({ hit: 0, miss: 0 });
  const [thinkingEnabled, setThinkingEnabled] = useState(true);
  const [agentStatus, setAgentStatus] = useState<AgentStatus>("ready");
  const [pendingApproval, setPendingApproval] = useState<ApprovalRequest | null>(null);
  const [theme, setTheme] = useState<"dark" | "light">(
    () => (typeof localStorage !== "undefined" && localStorage.getItem("dp-theme") as "dark" | "light") || "dark",
  );

  // Enhanced: providers, MCP, config, mode/effort
  const [providers, setProviders] = useState<ProviderSummary[]>([]);
  const [mcpServers, setMcpServers] = useState<McpServer[]>([]);
  const [config, setConfig] = useState<AppConfig>(DEFAULT_CONFIG);
  const [mode, setMode] = useState<Mode>("act");
  const [effort, setEffort] = useState<Effort>("high");
  const [autoMode, setAutoMode] = useState(false);
  const [currentModel, setCurrentModel] = useState<string>("");

  // streaming refs — accumulate text without spawning re-renders per token
  const streamingText = useRef("");
  const streamingMsgId = useRef("");
  const streamingReasoning = useRef("");
  const streamingReasoningMsgId = useRef("");
  const promptHistory = useRef<string[]>([]);
  const promptHistoryIdx = useRef(-1);
  const savedDraft = useRef("");
  const threadEndRef = useRef<HTMLDivElement>(null);

  const [sessions, setSessions] = useState<SessionSummary[]>([]);
  const [contextFiles, setContextFiles] = useState<ContextFile[]>([]);
  const [modifiedFiles] = useState<ContextFile[]>([]);

  useEffect(() => {
    getCapabilities()
      .then((c) => setCapabilities(c as unknown as Capabilities))
      .catch(console.error);
    listSkills().then(setSkills).catch(console.error);
    getWorkspaceFiles()
      .then((files) =>
        setContextFiles(files.map((f) => ({ path: f, changeType: undefined }))),
      )
      .catch(console.error);
    listSessions()
      .then((list) =>
        setSessions(list.map((s) => ({ id: s.id, title: s.title, active: false }))),
      )
      .catch(console.error);
    // Enhanced: load providers, MCP, config
    listProviders().then(setProviders).catch(console.error);
    listMcpServers().then(setMcpServers).catch(console.error);
    getConfig().then((s) => {
      try {
        const c = JSON.parse(s);
        setConfig({ ...DEFAULT_CONFIG, ...c });
        setMode(c.default_mode || "act");
        setEffort(c.default_effort || "high");
        setThinkingEnabled(c.thinking_enabled ?? true);
        setAutoMode(c.auto_mode ?? false);
      } catch { /* ignore parse errors */ }
    }).catch(console.error);
  }, []);

  // Apply theme on mount and on change
  useEffect(() => {
    document.documentElement.dataset.theme = theme;
    localStorage.setItem("dp-theme", theme);
  }, [theme]);

  const addMessage = useCallback((msg: Message) => setMessages((p) => [...p, msg]), []);
  const updateMessage = useCallback(
    (id: string, updater: (m: Message) => Message) =>
      setMessages((p) => p.map((m) => (m.id === id ? updater(m) : m))),
    [],
  );

  // auto-scroll thread to bottom on new content
  useEffect(() => {
    threadEndRef.current?.scrollIntoView({ behavior: "smooth" });
  }, [messages]);

  const handleSubmit = useCallback(async () => {
    const prompt = input.trim();
    if (!prompt || running) return;
    setInput("");
    setRunning(true);
    setAgentStatus("running");
    streamingText.current = "";
    streamingMsgId.current = "";
    streamingReasoning.current = "";
    streamingReasoningMsgId.current = "";
    addMessage({ id: uid(), role: "user", content: prompt });
    promptHistory.current.push(prompt);
    promptHistoryIdx.current = -1;
    savedDraft.current = "";

    const handlers = {
      onText(text: string) {
        // close out any open reasoning block
        if (streamingReasoningMsgId.current) {
          updateMessage(streamingReasoningMsgId.current, (m) => ({
            ...m,
            reasoningDone: true,
          }));
          streamingReasoningMsgId.current = "";
        }
        streamingText.current += text;
        if (!streamingMsgId.current) {
          streamingMsgId.current = uid();
          addMessage({
            id: streamingMsgId.current,
            role: "assistant",
            content: "",
          });
        }
        updateMessage(streamingMsgId.current, (m) => ({
          ...m,
          content: streamingText.current,
        }));
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
          ...m,
          content: m.content + argsDelta,
          toolArgs: (m.toolArgs ?? "") + argsDelta,
        }));
      },
      onToolCallEnd(id: string, name: string, args_: string) {
        updateMessage(id, (m) => ({
          ...m,
          toolName: name,
          content: args_,
          toolArgs: args_,
        }));
      },
      onToolResult(callId: string, result: string) {
        updateMessage(callId, (m) => ({ ...m, toolResult: result }));
      },
      onUsage(usage: UsageInfo) {
        setLastUsage(usage);
        setSessionCache((p) => ({
          hit: p.hit + usage.cache_hit_tokens,
          miss: p.miss + usage.cache_miss_tokens,
        }));
      },
      onDone(text: string) {
        if (streamingReasoningMsgId.current) {
          updateMessage(streamingReasoningMsgId.current, (m) => ({
            ...m,
            reasoningDone: true,
          }));
          streamingReasoningMsgId.current = "";
        }
        if (text && streamingMsgId.current) {
          updateMessage(streamingMsgId.current, (m) => ({ ...m, content: text }));
        }
        streamingMsgId.current = "";
        setRunning(false);
        setAgentStatus("ready");
      },
      onApprovalRequest(request: { id: string; title: string; description?: string }) {
        setPendingApproval(request);
      },
      onError(message: string) {
        addMessage({ id: uid(), role: "assistant", content: `Error: ${message}` });
        setRunning(false);
        setAgentStatus("ready");
      },
    };

    try {
      await submitPrompt(
        { prompt, reasoning_effort: effort, thinking_enabled: thinkingEnabled, mode, auto_mode: autoMode, max_steps: config.max_steps },
        handlers,
      );
    } catch (err) {
      addMessage({ id: uid(), role: "assistant", content: `Error: ${err}` });
      setRunning(false);
      setAgentStatus("ready");
    }
  }, [
    input,
    running,
    addMessage,
    updateMessage,
    effort,
    thinkingEnabled,
    mode,
    autoMode,
    config.max_steps,
  ]);

  const handleCancel = useCallback(async () => {
    await cancelRun();
    setRunning(false);
    setAgentStatus("ready");
  }, []);

  // Enhanced: model switching
  const handleModelSwitch = useCallback(async (provider: string, model: string) => {
    await switchModel(provider, model);
    setCurrentModel(model);
  }, []);

  // Enhanced: config save
  const handleSaveConfig = useCallback(async (newConfig: AppConfig) => {
    setConfig(newConfig);
    await saveConfig(newConfig);
  }, []);

  const handleNewSession = useCallback(async () => {
    if (running) return;
    await newSession();
    // Create a new session entry on the backend
    try {
      await createSession();
    } catch { /* ignore */ }
    try {
      const list = await listSessions();
      setSessions(list.map((s: any) => ({ id: s.id, title: s.title, active: false })));
    } catch { /* ignore */ }
    setMessages([]);
    setLastUsage(null);
    setSessionCache({ hit: 0, miss: 0 });
    setPendingApproval(null);
    streamingText.current = "";
    streamingMsgId.current = "";
    streamingReasoning.current = "";
    streamingReasoningMsgId.current = "";
  }, [running]);

  // prompt history navigation (Cmd+Up / Cmd+Down)
  const handleComposerKeyDown = useCallback(
    (e: React.KeyboardEvent) => {
      const mod = e.metaKey || e.ctrlKey;
      if (mod && e.key === "ArrowUp") {
        e.preventDefault();
        const hist = promptHistory.current;
        if (hist.length > 0 && promptHistoryIdx.current < hist.length - 1) {
          if (promptHistoryIdx.current === -1) savedDraft.current = input;
          promptHistoryIdx.current++;
          setInput(hist[hist.length - 1 - promptHistoryIdx.current]);
        }
        return;
      }
      if (mod && e.key === "ArrowDown") {
        e.preventDefault();
        if (promptHistoryIdx.current > 0) {
          promptHistoryIdx.current--;
          setInput(
            promptHistory.current[
              promptHistory.current.length - 1 - promptHistoryIdx.current
            ],
          );
        } else if (promptHistoryIdx.current === 0) {
          promptHistoryIdx.current = -1;
          setInput(savedDraft.current);
        }
        return;
      }
      if (e.key === "Enter" && !e.shiftKey) {
        e.preventDefault();
        if (running) handleCancel();
        else handleSubmit();
      }
    },
    [input, running, handleSubmit, handleCancel],
  );

  const cachePct =
    sessionCache.hit + sessionCache.miss > 0
      ? Math.round((sessionCache.hit / (sessionCache.hit + sessionCache.miss)) * 100)
      : 0;

  const activeProvider = providers.find(p => p.connected) || providers[0];
  const displayModel = currentModel || activeProvider?.model || "deepseek-v4";

  return (
    <div
      className="dp-shell"
      data-ctx-collapsed={contextCollapsed}
      data-side-collapsed={sideCollapsed}
    >
      <TitleBar
        title="DPronix"
        thinkingLabel={running ? "thinking" : undefined}
        effort={effort}
        sideCollapsed={sideCollapsed}
        onToggleSidebar={() => setSideCollapsed((v) => !v)}
        onToggleContext={() => setContextCollapsed((v) => !v)}
        onOpenSettings={() => setShowSettings((v) => !v)}
      />

      <SidebarPanel
        sessions={sessions}
        skills={skills}
        providers={providers}
        mcpServers={mcpServers}
        collapsed={sideCollapsed}
        onNewSession={handleNewSession}
        running={running}
        messageCount={messages.length}
      />

      <Suspense fallback={null}>
        <ModeBar
          mode={mode}
          effort={effort}
          thinking={thinkingEnabled}
          autoMode={autoMode}
          onModeChange={setMode}
          onEffortChange={setEffort}
          onThinkingChange={setThinkingEnabled}
          onAutoModeChange={setAutoMode}
          running={running}
          caps={capabilities}
        />
      </Suspense>

      <main className="dp-main">
        <div className="dp-thread">
          <Transcript
            messages={messages}
            running={running}
            pendingApproval={pendingApproval}
            onApprove={() => {
              if (pendingApproval) {
                respondApproval(pendingApproval.id, true);
                setPendingApproval(null);
              }
            }}
            onReject={() => {
              if (pendingApproval) {
                respondApproval(pendingApproval.id, false);
                setPendingApproval(null);
              }
            }}
            endRef={threadEndRef}
          />
        </div>

        <Composer
          value={input}
          onChange={setInput}
          onSubmit={handleSubmit}
          onCancel={handleCancel}
          onKeyDown={handleComposerKeyDown}
          running={running}
          placeholder="Ask anything… (Enter to send, Shift+Enter for new line)"
        />
      </main>

      <ContextPanel
        files={contextFiles}
        modified={modifiedFiles}
        memoryCount={3}
        collapsed={contextCollapsed}
      />

      <StatusBar
        model={displayModel}
        mode={mode}
        tokensUp={lastUsage?.prompt_tokens ?? 0}
        tokensDown={lastUsage?.completion_tokens ?? 0}
        cachePercent={cachePct}
        cacheHit={sessionCache.hit}
        status={agentStatus}
      />

      {showSettings && (
        <SettingsPanel
          config={config}
          providers={providers}
          mcpServers={mcpServers}
          thinkingEnabled={thinkingEnabled}
          onToggleThinking={() => setThinkingEnabled((v: boolean) => !v)}
          effort={effort}
          onEffortChange={setEffort}
          effortLevels={
            capabilities?.reasoning_effort_levels ?? ["low", "medium", "high"]
          }
          theme={theme}
          onThemeChange={setTheme}
          onSave={handleSaveConfig}
          onNewSession={handleNewSession}
          onClose={() => setShowSettings(false)}
        />
      )}
    </div>
  );
}
