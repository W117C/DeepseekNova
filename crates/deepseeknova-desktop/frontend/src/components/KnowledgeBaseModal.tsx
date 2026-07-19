/**
 * KnowledgeBaseModal.tsx — 知识库管理弹窗
 * 显示已配置的知识库，支持添加、删除、启用/禁用
 */

import { useState } from "react";

// 知识库数据结构
export interface KnowledgeBase {
  id: string;
  name: string;
  description: string;
  enabled: boolean;
  type: "builtin" | "user";
  path?: string;
}

// 模拟知识库数据
const mockKnowledgeBases: KnowledgeBase[] = [
  {
    id: "1",
    name: "DeepseekNova 项目文档",
    description: "项目架构、API 文档、开发指南",
    enabled: true,
    type: "user",
  },
  {
    id: "2",
    name: "Rust 最佳实践",
    description: "Rust 编程规范、性能优化、错误处理",
    enabled: true,
    type: "builtin",
  },
  {
    id: "3",
    name: "React 开发指南",
    description: "组件设计、Hooks、性能优化",
    enabled: false,
    type: "builtin",
  },
];

export default function KnowledgeBaseModal({ onClose }: { onClose: () => void }) {
  const [knowledgeBases, setKnowledgeBases] = useState<KnowledgeBase[]>(mockKnowledgeBases);
  const [searchQuery, setSearchQuery] = useState("");
  const [filter, setFilter] = useState<"all" | "builtin" | "user" | "enabled">("all");
  const [selectedIds, setSelectedIds] = useState<Set<string>>(new Set());

  const filteredKBs = knowledgeBases.filter((kb) => {
    const matchesSearch =
      kb.name.toLowerCase().includes(searchQuery.toLowerCase()) ||
      kb.description.toLowerCase().includes(searchQuery.toLowerCase());

    if (filter === "all") return matchesSearch;
    if (filter === "builtin") return matchesSearch && kb.type === "builtin";
    if (filter === "user") return matchesSearch && kb.type === "user";
    if (filter === "enabled") return matchesSearch && kb.enabled;
    return matchesSearch;
  });

  const toggleEnabled = (id: string) => {
    setKnowledgeBases(knowledgeBases.map((kb) => (kb.id === id ? { ...kb, enabled: !kb.enabled } : kb)));
  };

  const toggleSelect = (id: string) => {
    const newSelected = new Set(selectedIds);
    if (newSelected.has(id)) {
      newSelected.delete(id);
    } else {
      newSelected.add(id);
    }
    setSelectedIds(newSelected);
  };

  const deleteSelected = () => {
    setKnowledgeBases(knowledgeBases.filter((kb) => !selectedIds.has(kb.id)));
    setSelectedIds(new Set());
  };

  return (
    <div className="modal-overlay" onClick={onClose}>
      <div className="modal" onClick={(e) => e.stopPropagation()}>
        <div className="modal-header">
          <div className="modal-title">
            <span className="icon-only">📚</span>
            <span className="text-only">知识库管理</span>
          </div>
          <button className="btn-icon" onClick={onClose}>
            ✕
          </button>
        </div>

        <div className="modal-body">
          {/* 搜索和筛选 */}
          <div style={{ marginBottom: "16px" }}>
            <input
              type="text"
              placeholder="搜索知识库..."
              value={searchQuery}
              onChange={(e) => setSearchQuery(e.target.value)}
              style={{
                width: "100%",
                padding: "8px 12px",
                background: "var(--bg-1)",
                border: "1px solid var(--border-1)",
                borderRadius: "var(--radius-sm)",
                color: "var(--text-1)",
                fontSize: "13px",
              }}
            />
          </div>

          {/* 筛选标签 */}
          <div style={{ display: "flex", gap: "8px", marginBottom: "16px", flexWrap: "wrap" }}>
            {[
              { key: "all", label: "全部", icon: "📋" },
              { key: "builtin", label: "内置", icon: "📦" },
              { key: "user", label: "用户", icon: "👤" },
              { key: "enabled", label: "启用", icon: "✓" },
            ].map((f) => (
              <button
                key={f.key}
                className={`tag ${filter === f.key ? "tag-active" : ""}`}
                onClick={() => setFilter(f.key as any)}
                style={{ cursor: "pointer" }}
              >
                <span className="icon-only">{f.icon}</span>
                <span className="text-only">{f.label}</span>
              </button>
            ))}
          </div>

          {/* 知识库列表 */}
          <div style={{ display: "flex", flexDirection: "column", gap: "8px", marginBottom: "16px" }}>
            {filteredKBs.length === 0 ? (
              <div style={{ textAlign: "center", padding: "40px 20px", color: "var(--text-3)" }}>
                <div style={{ fontSize: "48px", marginBottom: "8px", opacity: 0.3 }}>📚</div>
                <div>没有找到知识库</div>
              </div>
            ) : (
              filteredKBs.map((kb) => (
                <div
                  key={kb.id}
                  className="card"
                  style={{
                    padding: "12px",
                    cursor: "pointer",
                    border: selectedIds.has(kb.id) ? "1px solid var(--accent)" : "1px solid var(--border-1)",
                  }}
                  onClick={() => toggleSelect(kb.id)}
                >
                  <div style={{ display: "flex", alignItems: "center", gap: "8px", marginBottom: "4px" }}>
                    <div style={{ fontSize: "18px" }}>{kb.type === "builtin" ? "📦" : "📄"}</div>
                    <div style={{ flex: 1 }}>
                      <div style={{ fontSize: "14px", fontWeight: "500", color: "var(--text-1)" }}>{kb.name}</div>
                      <div style={{ fontSize: "12px", color: "var(--text-3)" }}>{kb.description}</div>
                    </div>
                    <label className="toggle-switch" onClick={(e) => e.stopPropagation()}>
                      <input type="checkbox" checked={kb.enabled} onChange={() => toggleEnabled(kb.id)} />
                      <span className="toggle-slider"></span>
                    </label>
                  </div>
                </div>
              ))
            )}
          </div>

          {/* 操作按钮 */}
          <div style={{ display: "flex", gap: "8px" }}>
            <button className="btn btn-primary" style={{ flex: 1 }}>
              <span className="icon-only">+</span>
              <span className="text-only">创建知识库</span>
            </button>
            {selectedIds.size > 0 && (
              <button className="btn btn-danger" onClick={deleteSelected}>
                <span className="icon-only">🗑️</span>
                <span className="text-only">删除选中 ({selectedIds.size})</span>
              </button>
            )}
          </div>
        </div>

        <div className="modal-footer">
          <button className="btn" onClick={onClose}>
            关闭
          </button>
        </div>
      </div>
    </div>
  );
}
