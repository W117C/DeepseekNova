/**
 * Transcript.tsx — 消息流（react-virtuoso 虚拟化 + 流式跟随滚动）
 * 历史消息 React.memo 隔离重渲；审批卡/加载指示挂在 Footer；
 * 长对话导航：回到顶部 FAB + 消息大纲（点击跳转到对应用户消息）。
 */

import { memo, useRef, useState } from "react";
import { Virtuoso, type VirtuosoHandle } from "react-virtuoso";
import { useStore } from "../store";
import { useI18n } from "../i18n";
import type { Message } from "../types";
import MessageItem from "./MessageItem";
import ToolCard from "./ToolCard";
import ReasoningCard from "./ReasoningCard";
import ApprovalCard from "./ApprovalCard";
import ErrorBar from "./ErrorBar";
import PhaseIndicator from "./PhaseIndicator";
import Welcome from "./Welcome";

const Row = memo(function Row({ msg }: { msg: Message }) {
  if (msg.role === "reasoning") return <ReasoningCard message={msg} />;
  if (msg.role === "tool") return <ToolCard message={msg} />;
  if (msg.role === "error") return <ErrorBar message={msg} />;
  return <MessageItem message={msg} />;
});

function Footer() {
  const { t } = useI18n();
  const pendingApproval = useStore((s) => s.pendingApproval);
  const running = useStore((s) => s.running);
  const phase = useStore((s) => s.phase);
  const runElapsedMs = useStore((s) => s.runElapsedMs);
  return (
    <div style={{ padding: "0 12px 10px" }}>
      {pendingApproval && <ApprovalCard approval={pendingApproval} />}
      {running && <PhaseIndicator />}
      {!running && runElapsedMs !== null && (phase === "done" || phase === "stopped") && (
        <div className="phase done thread-inset">
          {phase === "stopped" ? t("phase.stopped") : t("phase.done")} · {t("phase.elapsed")}{" "}
          {(runElapsedMs / 1000).toFixed(1)}s
        </div>
      )}
    </div>
  );
}

export default function Transcript() {
  const { t } = useI18n();
  const messages = useStore((s) => s.messages);
  const running = useStore((s) => s.running);
  const virtuosoRef = useRef<VirtuosoHandle>(null);
  const [atTop, setAtTop] = useState(true);
  const [outlineOpen, setOutlineOpen] = useState(false);

  if (messages.length === 0 && !running) {
    return (
      <div className="transcript">
        <Welcome />
      </div>
    );
  }

  const userEntries = messages
    .map((m, i) => ({ m, i }))
    .filter(({ m }) => m.role === "user");

  const jumpTo = (index: number) => {
    virtuosoRef.current?.scrollToIndex({ index, align: "start", behavior: "smooth" });
    setOutlineOpen(false);
  };

  return (
    <div className="thread-wrap">
      <Virtuoso
        ref={virtuosoRef}
        className="transcript"
        style={{ flex: 1 }}
        data={messages}
        computeItemKey={(_, m) => m.id}
        itemContent={(_, m) => (
          <div style={{ padding: "0 12px" }}>
            <Row msg={m} />
          </div>
        )}
        followOutput={(atBottom) => (atBottom ? "smooth" : false)}
        atTopStateChange={setAtTop}
        components={{ Footer }}
        increaseViewportBy={{ top: 200, bottom: 400 }}
      />

      {/* 悬浮导航：消息大纲 + 回到顶部 */}
      <div className="fabs">
        <button
          className={`fab ${userEntries.length > 0 ? "show" : ""}`}
          title={t("nav.outline")}
          onClick={() => setOutlineOpen((o) => !o)}
        >
          ≡
        </button>
        <button
          className={`fab ${!atTop ? "show" : ""}`}
          title={t("nav.backToTop")}
          onClick={() => {
            virtuosoRef.current?.scrollToIndex({ index: 0, behavior: "smooth" });
            setOutlineOpen(false);
          }}
        >
          ↑
        </button>
      </div>
      {outlineOpen && (
        <div className="outline-pop">
          <div className="oh">{t("nav.outline")}</div>
          {userEntries.length === 0 && <div className="oi">{t("nav.noMessages")}</div>}
          {userEntries.map(({ m, i }, n) => (
            <div key={m.id} className="oi" onClick={() => jumpTo(i)}>
              {n + 1}. {m.content.trim().slice(0, 40)}
            </div>
          ))}
        </div>
      )}
    </div>
  );
}
