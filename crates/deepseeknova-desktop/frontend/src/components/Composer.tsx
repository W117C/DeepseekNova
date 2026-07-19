/**
 * Composer.tsx — 输入区
 * 多行输入 + 文件附件 + 发送/停止 + 上下文指示器 + Slash 命令
 */

import { useState, useRef, useEffect, useCallback } from "react";
import { useStore, slashCommands } from "../store";
import { submitPrompt, cancelRun } from "../bridge";

export default function Composer() {
  const input = useStore((s) => s.input);
  const setInput = useStore((s) => s.setInput);
  const running = useStore((s) => s.running);
  const setRunning = useStore((s) => s.setRunning);
  const mode = useStore((s) => s.mode);
  const effort = useStore((s) => s.effort);
  const model = useStore((s) => s.model);
  const addMessage = useStore((s) => s.addMessage);
  const updateMessage = useStore((s) => s.updateMessage);
  const lastUsage = useStore((s) => s.lastUsage);
  const sessionCache = useStore((s) => s.sessionCache);

  const [showSlash, setShowSlash] = useState(false);
  const [slashIndex, setSlashIndex] = useState(0);
  const textareaRef = useRef<HTMLTextAreaElement>(null);
  const streamingText = useRef("");
  const streamingMsgId = useRef("");
  const streamingReasoningId = useRef("");

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

  // 上下文窗口使用率
  const totalCache = sessionCache.hit + sessionCache.miss;
  const cacheRate = totalCache > 0 ? Math.round((sessionCache.hit / totalCache) * 100) : 0;
  const contextPct = lastUsage
    ? Math.min(100, (lastUsage.total_tokens / 64000) * 100)
    : 0;
  const contextColor =
    contextPct > 80 ? "var(--red)" : contextPct > 50 ? "var(--amber)" : "var(--green)";

  const handleSubmit = useCallback(async () => {
    const prompt = input.trim();
    if (!prompt || running) return;
    setInput("");
    setRunning(true);
    setShowSlash(false);
    streamingText.current = "";
    streamingMsgId.current = "";

    addMessage({ id: crypto.randomUUID(), role: "user", content: prompt });

    const handlers = {
      onText(text: string) {
        streamingText.current += text;
        if (!streamingMsgId.current) {
          streamingMsgId.current = crypto.randomUUID();
          addMessage({ id: streamingMsgId.current, role: "assistant", content: "" });
        }
        updateMessage(streamingMsgId.current, (m) => ({ ...m, content: streamingText.current }));
      },
      onReasoning(text: string) {
        if (!streamingReasoningId.current) {
          streamingReasoningId.current = crypto.randomUUID();
          addMessage({ id: streamingReasoningId.current, role: "reasoning", content: text, reasoningDone: false });
        } else {
          updateMessage(streamingReasoningId.current, (m) => ({ ...m, content: m.content + text }));
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
      onToolCallEnd(id: string, name: string, arguments_: string) {
        updateMessage(id, (m) => ({ ...m, toolName: name, content: arguments_, toolArgs: arguments_ }));
      },
      onToolResult(callId: string, result: string) {
        updateMessage(callId, (m) => ({ ...m, toolResult: result }));
      },
      onTurnComplete() {
        if (streamingReasoningId.current) {
          updateMessage(streamingReasoningId.current, (m) => ({ ...m, reasoningDone: true }));
          streamingReasoningId.current = "";
        }
      },
      onUsage(usage: any) {
        useStore.getState().setLastUsage(usage);
        useStore.getState().addCacheTokens(usage.cache_hit_tokens, usage.cache_miss_tokens);
      },
      onDone(text: string) {
        if (streamingReasoningId.current) {
          updateMessage(streamingReasoningId.current, (m) => ({ ...m, reasoningDone: true }));
          streamingReasoningId.current = "";
        }
        if (text && streamingMsgId.current) {
          updateMessage(streamingMsgId.current, (m) => ({ ...m, content: text }));
        }
        streamingMsgId.current = "";
        setRunning(false);
      },
      onError(message: string) {
        addMessage({ id: crypto.randomUUID(), role: "assistant", content: `⚠️ Error: ${message}` });
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
        },
        handlers
      );
    } catch (err) {
      addMessage({ id: crypto.randomUUID(), role: "assistant", content: `⚠️ Error: ${err}` });
      setRunning(false);
    }
  }, [input, running, mode, effort, model, addMessage, updateMessage, setInput, setRunning]);

  const handleCancel = useCallback(async () => {
    await cancelRun();
    setRunning(false);
  }, [setRunning]);

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
        placeholder={running ? "Agent 运行中…" : "输入消息，按 Enter 发送，Shift+Enter 换行，/ 触发命令…"}
        value={input}
        onChange={(e) => handleChange(e.target.value)}
        onKeyDown={handleKeyDown}
        disabled={running}
        rows={1}
      />

      <div className="composer-footer">
        {/* 上下文窗口指示器 */}
        <div className="context-ring" title="上下文窗口使用率">
          {lastUsage && (
            <>
              <span>{lastUsage.total_tokens.toLocaleString()} tokens</span>
              <div className="context-ring-bar">
                <div
                  className="context-ring-fill"
                  style={{ width: `${contextPct}%`, background: contextColor }}
                />
              </div>
            </>
          )}
          {totalCache > 0 && (
            <span title="缓存命中率">💡 {cacheRate}%</span>
          )}
        </div>

        <div className="spacer" />

        {/* 模式提示 */}
        <span className="tag" style={{ opacity: 0.7 }}>
          <span className="icon-only">
            {mode === "plan" && "🔒 Plan"}
            {mode === "act" && "✋ Act"}
            {mode === "yolo" && "🚀 YOLO"}
          </span>
          <span className="text-only">
            模式: {mode.toUpperCase()}
          </span>
        </span>

        {/* 发送/停止按钮 */}
        {running ? (
          <button className="btn btn-danger" onClick={handleCancel}>
            <svg width="14" height="14" viewBox="0 0 24 24" fill="currentColor">
              <rect x="6" y="6" width="12" height="12" rx="2"/>
            </svg>
            停止
          </button>
        ) : (
          <button
            className="btn btn-primary"
            onClick={handleSubmit}
            disabled={!input.trim()}
          >
            <svg width="14" height="14" viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth="2">
              <line x1="22" y1="2" x2="11" y2="13"/>
              <polygon points="22 2 15 22 11 13 2 9 22 2"/>
            </svg>
            发送
          </button>
        )}
      </div>
    </div>
  );
}
