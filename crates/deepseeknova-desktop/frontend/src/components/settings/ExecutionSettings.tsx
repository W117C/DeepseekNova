import { useState } from "react";
import { SettingRow } from "./Shared";

export default function ExecutionSettings() {
  const [defaultMode, setDefaultMode] = useState("act");
  const [autoCommit, setAutoCommit] = useState(false);
  const [tokenBudget, setTokenBudget] = useState(500000);
  const [budgetAlert, setBudgetAlert] = useState(5);
  const [maxRetries, setMaxRetries] = useState(4);
  const [timeout, setTimeout] = useState(120);

  return (
    <div style={{ display: "flex", flexDirection: "column", gap: 0 }}>
      <SettingRow label="默认执行模式" desc="新会话的默认模式">
        <select className="input" value={defaultMode} onChange={(e) => setDefaultMode(e.target.value)} style={{ width: 220 }}>
          <option value="plan">📋 Plan（只读审计）</option>
          <option value="act">✋ Act（写操作需审批）</option>
          <option value="yolo">🚀 YOLO（全自动）</option>
        </select>
      </SettingRow>
      <SettingRow label="自动提交" desc="Agent 完成任务后自动 git commit">
        <label className="toggle-switch"><input type="checkbox" checked={autoCommit} onChange={(e) => setAutoCommit(e.target.checked)} /><span className="toggle-slider"></span></label>
      </SettingRow>
      <SettingRow label="Token 预算" desc="单会话 Token 上限">
        <input type="number" className="input" value={tokenBudget} onChange={(e) => setTokenBudget(Number(e.target.value))} style={{ width: 120 }} />
      </SettingRow>
      <SettingRow label="预算告警" desc={`费用超过 $${budgetAlert} 时提醒`}>
        <input type="number" className="input" value={budgetAlert} onChange={(e) => setBudgetAlert(Number(e.target.value))} style={{ width: 80 }} step="0.5" />
      </SettingRow>
      <SettingRow label="最大重试次数" desc="工具调用失败后自动重试次数">
        <input type="number" className="input" value={maxRetries} onChange={(e) => setMaxRetries(Number(e.target.value))} style={{ width: 80 }} />
      </SettingRow>
      <SettingRow label="执行超时" desc="单次工具执行超时（秒）">
        <input type="number" className="input" value={timeout} onChange={(e) => setTimeout(Number(e.target.value))} style={{ width: 80 }} />
      </SettingRow>
    </div>
  );
}

