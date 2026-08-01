/**
 * PhaseIndicator.tsx — AI 工作计时指示（mockup 定稿）
 * 思考中(提交后) → 推理中(首个 reasoning_delta) → 回复中(首个 text_delta)
 * 100ms 秒表为组件局部 state，不进全局 store。
 * 完成/停止后由 Composer 落入静态文本（本组件只在 running 时渲染）。
 */

import { useEffect, useState } from "react";
import { useStore } from "../store";
import { useI18n } from "../i18n";

export default function PhaseIndicator() {
  const { t } = useI18n();
  const phase = useStore((s) => s.phase);
  const runStartTs = useStore((s) => s.runStartTs);
  const [now, setNow] = useState(() => Date.now());

  useEffect(() => {
    const timer = setInterval(() => setNow(Date.now()), 100);
    return () => clearInterval(timer);
  }, []);

  if (!runStartTs) return null;
  const label =
    phase === "reasoning" ? t("phase.reasoning")
      : phase === "replying" ? t("phase.replying")
      : t("phase.thinking");
  const secs = ((now - runStartTs) / 1000).toFixed(1);

  return (
    <div className="phase thread-inset">
      <span className="ring act sm" />
      <span>{label}</span>
      <b>{secs}s</b>
    </div>
  );
}
