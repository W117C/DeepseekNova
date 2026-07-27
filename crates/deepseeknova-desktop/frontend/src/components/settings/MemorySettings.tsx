/**
 * MemorySettings.tsx — 记忆：长期记忆管理（get/add/delete_memory · SQLite FTS5）
 */

import { useEffect, useState } from "react";
import { getMemories, addMemory, deleteMemory, type MemoryEntry } from "../../bridge";
import { SectionHeader, SettingRow, Toggle } from "./Shared";

export default function MemorySettings() {
  const [memories, setMemories] = useState<MemoryEntry[]>([]);
  const [draft, setDraft] = useState("");

  const refresh = () => getMemories().then(setMemories).catch(() => {});
  useEffect(() => { refresh(); }, []);

  const add = async () => {
    const text = draft.trim();
    if (!text) return;
    try {
      await addMemory("user", text);
      setDraft("");
      refresh();
    } catch { /* ignore */ }
  };

  const remove = async (id: string) => {
    try {
      await deleteMemory(id);
      refresh();
    } catch { /* ignore */ }
  };

  return (
    <div>
      <SectionHeader title="记忆" desc="get / add / delete_memory · SQLite FTS5" />

      {memories.map((m) => (
        <SettingRow key={m.id} label={m.memory_type} desc={m.text}>
          <button className="btn btn-danger" onClick={() => remove(m.id)}>删除</button>
        </SettingRow>
      ))}
      {memories.length === 0 && (
        <SettingRow label="暂无长期记忆" desc="记忆将在任务中自动沉淀，或在下方手动添加">
          <span className="tag">—</span>
        </SettingRow>
      )}

      <div style={{ display: "flex", gap: 8, margin: "12px 0" }}>
        <input
          className="input"
          placeholder="新增记忆内容…"
          value={draft}
          onChange={(e) => setDraft(e.target.value)}
          onKeyDown={(e) => e.key === "Enter" && add()}
        />
        <button className="btn btn-primary" onClick={add} disabled={!draft.trim()}>＋ 添加</button>
      </div>

      <SettingRow label="写入策略" desc="何时写入长期记忆">
        <span className="tag">任务完成时</span>
      </SettingRow>
      <SettingRow label="会话隔离" desc="短期记忆跨会话不共享">
        <Toggle checked onChange={() => {}} />
      </SettingRow>
    </div>
  );
}
