/**
 * ErrorBar.tsx — 错误红条（mockup 定稿：左红边 + 忽略按钮；重试一期隐藏）
 */

import { useState } from "react";
import { useI18n } from "../i18n";
import type { Message } from "../types";

export default function ErrorBar({ message }: { message: Message }) {
  const { t } = useI18n();
  const [dismissed, setDismissed] = useState(false);
  if (dismissed) return null;

  return (
    <div className="err-bar thread-inset">
      <span>⚠ {message.content}</span>
      <span className="e-a">
        <button className="ghost" onClick={() => setDismissed(true)}>{t("error.dismiss")}</button>
      </span>
    </div>
  );
}
