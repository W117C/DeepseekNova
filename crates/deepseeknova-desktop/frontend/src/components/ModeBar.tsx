/**
 * ModeBar.tsx — 模式切换（mockup 定稿四档：代理 / 对话 / 规划 / 审查）
 */

import { useStore } from "../store";
import { useI18n } from "../i18n";
import type { Mode } from "../types";

const modes: Mode[] = ["agent", "chat", "plan", "review"];

export default function ModeBar() {
  const { t } = useI18n();
  const mode = useStore((s) => s.mode);
  const setMode = useStore((s) => s.setMode);

  return (
    <div className="seg" title="Agent 模式">
      {modes.map((m) => (
        <button
          key={m}
          className={mode === m ? "on" : ""}
          onClick={() => setMode(m)}
          title={t(`mode.${m}.title`)}
        >
          {t(`mode.${m}`)}
        </button>
      ))}
    </div>
  );
}
