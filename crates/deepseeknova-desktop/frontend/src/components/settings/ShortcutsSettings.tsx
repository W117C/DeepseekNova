

export default function ShortcutsSettings() {
  const shortcuts = [
    { action: "发送消息", keys: "Enter", category: "输入" },
    { action: "换行", keys: "Shift + Enter", category: "输入" },
    { action: "命令面板", keys: "Ctrl/Cmd + P", category: "全局" },
    { action: "中断生成", keys: "Esc", category: "对话" },
    { action: "新建会话", keys: "Ctrl/Cmd + N", category: "会话" },
    { action: "关闭标签", keys: "Ctrl/Cmd + W", category: "会话" },
    { action: "切换标签", keys: "Ctrl/Cmd + Tab", category: "会话" },
    { action: "搜索会话", keys: "Ctrl/Cmd + F", category: "搜索" },
    { action: "@ 引用文件", keys: "@", category: "输入" },
    { action: "/ 斜杠命令", keys: "/", category: "输入" },
    { action: "切换主题", keys: "Ctrl/Cmd + Shift + T", category: "全局" },
    { action: "折叠侧边栏", keys: "Ctrl/Cmd + B", category: "全局" },
    { action: "折叠右侧面板", keys: "Ctrl/Cmd + J", category: "全局" },
    { action: "切换显示模式", keys: "Ctrl/Cmd + Shift + M", category: "全局" },
    { action: "Plan 模式", keys: "Ctrl/Cmd + Shift + 1", category: "模式" },
    { action: "Act 模式", keys: "Ctrl/Cmd + Shift + 2", category: "模式" },
    { action: "YOLO 模式", keys: "Ctrl/Cmd + Shift + 3", category: "模式" },
    { action: "切换模型 Flash/Pro", keys: "Ctrl/Cmd + Shift + M", category: "模型" },
  ];

  const categories = [...new Set(shortcuts.map(s => s.category))];

  return (
    <div>
      {categories.map(cat => (
        <div key={cat} style={{ marginBottom: 12 }}>
          <div style={{ fontSize: 10, fontWeight: 600, textTransform: "uppercase", color: "var(--text-3)", letterSpacing: 0.5, marginBottom: 6 }}>{cat}</div>
          {shortcuts.filter(s => s.category === cat).map((s) => (
            <div key={s.action} style={{ display: "flex", alignItems: "center", justifyContent: "space-between", padding: "4px 0", borderBottom: "1px solid var(--border-1)" }}>
              <span style={{ fontSize: 12, color: "var(--text-1)" }}>{s.action}</span>
              <kbd className="tag" style={{ fontFamily: "var(--font-mono)", fontSize: 10, background: "var(--bg-3)", color: "var(--text-2)", padding: "2px 8px" }}>{s.keys}</kbd>
            </div>
          ))}
        </div>
      ))}
      <button className="btn btn-ghost" style={{ width: "100%", justifyContent: "center", fontSize: 11, marginTop: 8 }}>+ 自定义快捷键</button>
    </div>
  );
}

