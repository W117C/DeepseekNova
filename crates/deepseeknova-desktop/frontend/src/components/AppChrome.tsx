/**
 * AppChrome.tsx — 三栏布局外壳
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
        <Composer />
        <ControlBar />
      </main>
      <RightPanel />
      <StatusBar />

      {showSettings && <SettingsModal />}
      {showCommandPalette && <CommandPalette />}
    </div>
  );
}
