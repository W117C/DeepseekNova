/**
 * AppChrome.tsx — 布局外壳（四列挤压式 Grid）
 * 顶栏 + 侧栏(会话) + 对话区 + Diff 对比面板 + 任务抽屉 + 状态栏
 */

import { useStore } from "../store";
import Sidebar from "./Sidebar";
import Transcript from "./Transcript";
import Composer from "./Composer";
import ChipRow from "./ChipRow";
import TitleBar from "./TitleBar";
import StatusBar from "./StatusBar";
import SettingsModal from "./SettingsModal";
import CommandPalette from "./CommandPalette";
import DiffPanel from "./DiffPanel";
import TaskDrawer from "./TaskDrawer";

export default function AppChrome() {
  const sidebarCollapsed = useStore((s) => s.sidebarCollapsed);
  const diffOpen = useStore((s) => s.diffOpen);
  const drawerOpen = useStore((s) => s.drawerOpen);
  const showSettings = useStore((s) => s.showSettings);
  const showCommandPalette = useStore((s) => s.showCommandPalette);

  const shellClass = [
    "app-shell",
    sidebarCollapsed && "sidebar-collapsed",
    diffOpen && "diff-open",
    drawerOpen && "drawer-open",
  ]
    .filter(Boolean).join(" ");

  return (
    <div className={shellClass}>
      <TitleBar />
      <Sidebar />
      <main className="main-area">
        <Transcript />
        <div className="composer-zone">
          <ChipRow />
          <Composer />
        </div>
      </main>
      <DiffPanel />
      <TaskDrawer />
      <StatusBar />
      {showSettings && <SettingsModal />}
      {showCommandPalette && <CommandPalette />}
    </div>
  );
}
