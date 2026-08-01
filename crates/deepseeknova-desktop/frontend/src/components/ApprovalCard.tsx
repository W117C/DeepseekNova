/**
 * ApprovalCard.tsx — 内联审批卡（mockup 定稿：左橙边 + 命令代码块）
 */

import { useStore } from "../store";
import { useI18n } from "../i18n";
import { respondApproval } from "../bridge";
import type { ApprovalRequest } from "../types";

export default function ApprovalCard({ approval }: { approval: ApprovalRequest }) {
  const { t } = useI18n();
  const setPendingApproval = useStore((s) => s.setPendingApproval);

  const handleAction = async (action: "allow" | "deny") => {
    // 将审批结果通过 Tauri invoke 回传给后端，解除 agent 的阻塞等待
    await respondApproval(approval.id, action === "allow");
    setPendingApproval(null);
  };

  return (
    <div className="appr thread-inset">
      <div className="a-h">{t("approval.waiting")} · {approval.title}</div>
      <div className="a-s">{t("approval.hint")}</div>
      {approval.description && <div className="a-c">{approval.description}</div>}
      <div className="a-b">
        <button className="btn btn-primary" onClick={() => handleAction("allow")} title={t("approval.allow")}>
          {t("approval.allow")}
        </button>
        <button className="btn" onClick={() => handleAction("deny")} title={t("approval.deny")}>
          {t("approval.deny")}
        </button>
      </div>
    </div>
  );
}
