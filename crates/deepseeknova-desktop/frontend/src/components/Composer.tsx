/**
 * Composer.tsx — 输入区（mockup 定稿）
 * 附件 chip + 多行输入 + 底部工具条（附件/模型/思考程度/模式/语音/发送停止）
 */

import { useState, useRef, useEffect, useCallback } from "react";
import { open as openDialog } from "@tauri-apps/plugin-dialog";
import { useStore, slashCommands } from "../store";
import { submitPrompt, cancelRun } from "../bridge";
import { useI18n } from "../i18n";
import ModelSelector from "./ModelSelector";
import EffortSwitcher from "./EffortSwitcher";
import ModeBar from "./ModeBar";

export default function Composer() {
  const { t } = useI18n();
  const input = useStore((s) => s.input);
  const setInput = useStore((s) => s.setInput);
  const running = useStore((s) => s.running);
  const setRunning = useStore((s) => s.setRunning);
  const mode = useStore((s) => s.mode);
  const effort = useStore((s) => s.effort);
  const model = useStore((s) => s.model);
  const addMessage = useStore((s) => s.addMessage);
  const updateMessage = useStore((s) => s.updateMessage);
  const capabilities = useStore((s) => s.capabilities);
  const attachments = useStore((s) => s.attachments);
  const addAttachment = useStore((s) => s.addAttachment);
  const removeAttachment = useStore((s) => s.removeAttachment);
  const clearAttachments = useStore((s) => s.clearAttachments);
  const [recording, setRecording] = useState(false);

  const [showSlash, setShowSlash] = useState(false);
  const [slashIndex, setSlashIndex] = useState(0);
  const textareaRef = useRef<HTMLTextAreaElement>(null);
  const streamingText = useRef("");
  const streamingReasoning = useRef("");
  const streamingMsgId = useRef("");
  const streamingReasoningId = useRef("");
  const rafId = useRef<number | null>(null);

  // rAF 合帧：流式 delta 先累积到 ref，每帧最多写一次 store，
  // 配合 Transcript 的 React.memo，历史消息在流式期间零重渲染。
  const flushStream = useCallback(() => {
    rafId.current = null;
    const st = useStore.getState();
    if (streamingReasoningId.current) {
      const text = streamingReasoning.current;
      st.updateMessage(streamingReasoningId.current, (m) =>
        m.content === text ? m : { ...m, content: text }
      );
    }
    if (streamingMsgId.current) {
      const text = streamingText.current;
      st.updateMessage(streamingMsgId.current, (m) =>
        m.content === text ? m : { ...m, content: text }
      );
    }
  }, []);

  const scheduleFlush = useCallback(() => {
    if (rafId.current === null) {
      rafId.current = requestAnimationFrame(flushStream);
    }
  }, [flushStream]);

  // 自动调整高度
  useEffect(() => {
    if (textareaRef.current) {
      textareaRef.current.style.height = "auto";
      textareaRef.current.style.height = `${Math.min(textareaRef.current.scrollHeight, 200)}px`;
    }
  }, [input]);

  // Slash 命令过滤
  const filteredSlash = input.startsWith("/")
    ? slashCommands.filter((c) => c.name.startsWith(input.split(" ")[0]))
    : [];

  // 附件选择（tauri dialog）
  const pickFiles = useCallback(async () => {
    try {
      const picked = await openDialog({ multiple: true, title: "添加文件" });
      const paths = Array.isArray(picked) ? picked : picked ? [picked] : [];
      for (const p of paths) {
        const path = String(p);
        const name = path.split("/").pop() ?? path;
        addAttachment({ path, name });
      }
    } catch (err) {
      console.warn("attach_files dialog error:", err);
    }
  }, [addAttachment]);

  // 语音输入：一期 UI stub
  const toggleMic = useCallback(() => {
    setRecording((r) => {
      if (!r) setTimeout(() => setRecording(false), 3000);
      return !r;
    });
  }, []);

  const handleSubmit = useCallback(async () => {
    const prompt = input.trim();
    if (!prompt || running) return;
    setInput("");
    setRunning(true);
    setShowSlash(false);
    streamingText.current = "";
    streamingReasoning.current = "";
    streamingMsgId.current = "";
    streamingReasoningId.current = "";

    // Clear previous trace for this run
    useStore.getState().clearTrace();
    // AI 计时：思考中 → 推理中 → 回复中
    useStore.getState().markRunStart();

    addMessage({ id: crypto.randomUUID(), role: "user", content: prompt });

    const handlers = {
      onText(text: string) {
        useStore.getState().pushTraceEvent({ kind: "text_delta", text });
        streamingText.current += text;
        if (!streamingMsgId.current) {
          streamingMsgId.current = crypto.randomUUID();
          addMessage({ id: streamingMsgId.current, role: "assistant", content: "" });
          useStore.getState().markTtft();
        }
        scheduleFlush();
      },
      onReasoning(text: string) {
        useStore.getState().pushTraceEvent({ kind: "reasoning_delta", text, signature: null });
        streamingReasoning.current += text;
        if (!streamingReasoningId.current) {
          streamingReasoningId.current = crypto.randomUUID();
          addMessage({ id: streamingReasoningId.current, role: "reasoning", content: text, reasoningDone: false });
          useStore.getState().setPhase("reasoning");
        }
        scheduleFlush();
      },
      onToolCallStart(id: string, name: string) {
        useStore.getState().pushTraceEvent({ kind: "tool_call_start", id, name });
        useStore.getState().incToolCalls();
        addMessage({ id, role: "tool", content: "", toolName: name, toolId: id, startTs: Date.now() });
      },
      onToolCallDelta(id: string, argsDelta: string) {
        updateMessage(id, (m) => ({
          ...m,
          content: m.content + argsDelta,
          toolArgs: (m.toolArgs ?? "") + argsDelta,
        }));
      },
      onToolCallEnd(id: string, name: string, arguments_: string) {
        useStore.getState().pushTraceEvent({ kind: "tool_call_end", id, name, arguments: arguments_ });
        updateMessage(id, (m) => ({ ...m, toolName: name, content: arguments_, toolArgs: arguments_ }));
      },
      onToolResult(callId: string, result: string) {
        useStore.getState().pushTraceEvent({ kind: "tool_result", call_id: callId, result });
        updateMessage(callId, (m) => ({ ...m, toolResult: result, endTs: Date.now() }));
      },
      onVerification(ev: { command: string; passed: boolean; summary: string }) {
        useStore.getState().pushTraceEvent({ kind: "verification", ...ev });
      },
      onTurnComplete() {
        flushStream();
        if (streamingReasoningId.current) {
          updateMessage(streamingReasoningId.current, (m) => ({ ...m, reasoningDone: true }));
          streamingReasoningId.current = "";
          streamingReasoning.current = "";
        }
      },
      onUsage(usage: any) {
        useStore.getState().setLastUsage(usage);
        useStore.getState().addCacheTokens(usage.cache_hit_tokens, usage.cache_miss_tokens);
      },
      onDone(text: string) {
        useStore.getState().pushTraceEvent({ kind: "done", text, usage: useStore.getState().lastUsage });
        flushStream();
        if (streamingReasoningId.current) {
          updateMessage(streamingReasoningId.current, (m) => ({ ...m, reasoningDone: true }));
          streamingReasoningId.current = "";
          streamingReasoning.current = "";
        }
        if (text && streamingMsgId.current) {
          updateMessage(streamingMsgId.current, (m) => ({ ...m, content: text }));
        }
        streamingMsgId.current = "";
        useStore.getState().markRunEnd(false);
        setRunning(false);
      },
      onApprovalRequest(req: { id: string; title: string; description: string | null }) {
        // Surface the gate's Ask decision as an approval card; ApprovalCard
        // sends the user's answer back via respond_approval.
        useStore.getState().setPendingApproval(req);
      },
      onError(message: string) {
        useStore.getState().pushTraceEvent({ kind: "error", message });
        flushStream();
        addMessage({ id: crypto.randomUUID(), role: "error", content: message });
        useStore.getState().markRunEnd(false);
        setRunning(false);
      },
    };

    try {
      await submitPrompt(
        {
          prompt,
          model,
          reasoning_effort: effort,
          thinking_enabled: effort !== "low",
          agent_mode: mode,
          attachments: attachments.length ? attachments.map((a) => a.path) : undefined,
        },
        handlers
      );
      clearAttachments();
    } catch (err) {
      addMessage({ id: crypto.randomUUID(), role: "error", content: String(err) });
      useStore.getState().markRunEnd(false);
      setRunning(false);
    }
  }, [input, running, mode, effort, model, attachments, addMessage, updateMessage, setInput, setRunning, clearAttachments, flushStream, scheduleFlush]);

  const handleCancel = useCallback(async () => {
    await cancelRun();
    flushStream();
    useStore.getState().markRunEnd(true);
    setRunning(false);
  }, [setRunning, flushStream]);

  const handleKeyDown = (e: React.KeyboardEvent) => {
    // Enter 发送，Shift+Enter 换行
    if (e.key === "Enter" && !e.shiftKey) {
      e.preventDefault();
      if (showSlash && filteredSlash.length > 0) {
        const cmd = filteredSlash[slashIndex];
        cmd.action();
        setInput("");
        setShowSlash(false);
        return;
      }
      handleSubmit();
    }
    // Slash 命令导航
    if (showSlash && filteredSlash.length > 0) {
      if (e.key === "ArrowDown") {
        e.preventDefault();
        setSlashIndex((i) => (i + 1) % filteredSlash.length);
      } else if (e.key === "ArrowUp") {
        e.preventDefault();
        setSlashIndex((i) => (i - 1 + filteredSlash.length) % filteredSlash.length);
      } else if (e.key === "Escape") {
        setShowSlash(false);
      }
    }
    // Ctrl+P 打开命令面板
    if (e.ctrlKey && e.key === "p") {
      e.preventDefault();
      useStore.getState().setShowCommandPalette(true);
    }
  };

  const handleChange = (v: string) => {
    setInput(v);
    setShowSlash(v.startsWith("/"));
    setSlashIndex(0);
  };

  return (
    <div className="composer">
      {/* Slash 命令菜单 */}
      {showSlash && filteredSlash.length > 0 && (
        <div className="slash-menu">
          {filteredSlash.map((cmd, i) => (
            <div
              key={cmd.name}
              className={`slash-item ${i === slashIndex ? "selected" : ""}`}
              onClick={() => {
                cmd.action();
                setInput("");
                setShowSlash(false);
              }}
            >
              <span className="slash-item-name">{cmd.name}</span>
              <span className="slash-item-desc">{cmd.description}</span>
            </div>
          ))}
        </div>
      )}

      <textarea
        ref={textareaRef}
        className="composer-input"
        placeholder={running ? t("composer.placeholderRunning") : t("composer.placeholder")}
        value={input}
        onChange={(e) => handleChange(e.target.value)}
        onKeyDown={handleKeyDown}
        disabled={running}
        rows={1}
      />

      {/* 附件 chip 列表 */}
      {attachments.length > 0 && (
        <div className="atts">
          {attachments.map((a) => (
            <span key={a.path} className="att" title={a.path}>
              <span>{a.name}</span>
              <span className="x" onClick={() => removeAttachment(a.path)}>✕</span>
            </span>
          ))}
        </div>
      )}

      {/* 底部工具条（mockup cbar） */}
      <div className="cbar">
        {/* 添加文件 */}
        <button className="cib" title={t("composer.attach")} onClick={pickFiles}>
          <svg width="15" height="15" viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth="1.8" strokeLinecap="round">
            <path d="M21.4 11.05l-9.19 9.19a6 6 0 01-8.49-8.49l9.19-9.19a4 4 0 015.66 5.66l-9.2 9.19a2 2 0 01-2.83-2.83l8.49-8.48"/>
          </svg>
        </button>

        <ModelSelector />
        {capabilities?.supports_reasoning_effort !== false && <EffortSwitcher />}
        <ModeBar />

        <span className="spacer" />

        {/* 语音输入（一期 stub） */}
        <button
          className={`cib ${recording ? "rec" : ""}`}
          title={recording ? t("composer.voiceStop") : t("composer.voice")}
          onClick={toggleMic}
        >
          <svg width="15" height="15" viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth="1.8" strokeLinecap="round">
            <rect x="9" y="2" width="6" height="12" rx="3"/>
            <path d="M5 10v1a7 7 0 0014 0v-1M12 18v4"/>
          </svg>
        </button>

        {/* 发送/停止合一 */}
        {running ? (
          <button className="send-btn stop" onClick={handleCancel} title={t("app.stop")}>◼</button>
        ) : (
          <button className="send-btn" onClick={handleSubmit} disabled={!input.trim()} title={t("app.send")}>↑</button>
        )}
      </div>
    </div>
  );
}
