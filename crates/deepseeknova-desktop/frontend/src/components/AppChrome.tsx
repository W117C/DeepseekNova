/**
 * AppChrome.tsx — 三栏布局外壳（重构版）
 *
 * 参考 Reasonix + Hermes WebUI 的布局优点：
 * - 顶栏：多标签页（每个标签=独立会话）+ 面板控制
 * - 左侧：会话/技能/任务导航
 * - 中间：对话流 + 输入区 + 工具栏（紧凑分组）
 * - 右侧：文件面板（读/改分区）+ 上下文 + 记忆
 * - 底部：成本仪表盘（缓存率/Token/费用/时长）
 */

import { useStore } from "../store";
import Sidebar from "./Sidebar";
import RightPanel from "./RightPanel";
import Transcript from "./Transcript";
import Composer from "./Composer";
import ControlBar from "./ControlBar";
import TitleBar from "./TitleBar";
import StatusBar from "./StatusBar";
import SettingsModal from "./SettingsModal";
import CommandPalette from "./CommandPalette";

export default function AppChrome() {
  const sidebarCollapsed = useStore((s) => s.sidebarCollapsed);
  const rightCollapsed = useStore((s) => s.rightCollapsed);
  const showSettings = useStore((s) => s.showSettings);
  const showCommandPalette = useStore((s) => s.showCommandPalette);

  const shellClass = [
    "app-shell",
    sidebarCollapsed && "sidebar-collapsed",
    rightCollapsed && "right-collapsed",
  ].filter(Boolean).join(" ");

  return (
    <div className={shellClass}>
      <TitleBar />
      <Sidebar />
      <main className="main-area">
        <Transcript />
        <div className="composer-zone">
          <Composer />
          <ControlBar />
        </div>
      </main>
      <RightPanel />
      <StatusBar />

      {showSettings && <SettingsModal />}
      {showCommandPalette && <CommandPalette />}
    </div>
  );
}
