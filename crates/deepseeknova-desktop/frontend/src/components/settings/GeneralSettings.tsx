import { useState } from "react";
import { SettingRow } from "./Shared";

export default function GeneralSettings() {
  const [apiKey, setApiKey] = useState("sk-••••••••••••••••");
  const [baseUrl, setBaseUrl] = useState("https://api.deepseek.com");
  const [defaultModel, setDefaultModel] = useState("deepseek-v4-flash");
  const [language, setLanguage] = useState("zh-CN");
  const [fontSize, setFontSize] = useState(13);
  const [fontFamily, setFontFamily] = useState("system");
  const [autoSave, setAutoSave] = useState(true);
  const [tabRestore, setTabRestore] = useState(true);

  return (
    <div style={{ display: "flex", flexDirection: "column", gap: 0 }}>
      <SettingRow label="API Key" desc="DeepSeek API 密钥，在 platform.deepseek.com 申请">
        <input className="input" type="password" value={apiKey} onChange={(e) => setApiKey(e.target.value)} style={{ width: 260 }} />
      </SettingRow>
      <SettingRow label="Base URL" desc="API 基础地址，可替换为代理">
        <input className="input" value={baseUrl} onChange={(e) => setBaseUrl(e.target.value)} style={{ width: 260 }} />
      </SettingRow>
      <SettingRow label="默认模型" desc="新会话默认使用的模型">
        <select className="input" value={defaultModel} onChange={(e) => setDefaultModel(e.target.value)} style={{ width: 200 }}>
          <option value="deepseek-v4-flash">DeepSeek v4 Flash（快速）</option>
          <option value="deepseek-v4-pro">DeepSeek v4 Pro（高级推理）</option>
          <option value="deepseek-coder">DeepSeek Coder</option>
          <option value="deepseek-reasoner">DeepSeek Reasoner R1</option>
        </select>
      </SettingRow>
      <SettingRow label="语言" desc="界面语言">
        <select className="input" value={language} onChange={(e) => setLanguage(e.target.value)} style={{ width: 200 }}>
          <option value="zh-CN">简体中文</option>
          <option value="en-US">English</option>
        </select>
      </SettingRow>
      <SettingRow label="字体大小" desc={`当前: ${fontSize}px`}>
        <input type="range" min="11" max="18" value={fontSize} onChange={(e) => setFontSize(Number(e.target.value))} style={{ width: 200 }} />
      </SettingRow>
      <SettingRow label="字体家族" desc="界面字体">
        <select className="input" value={fontFamily} onChange={(e) => setFontFamily(e.target.value)} style={{ width: 200 }}>
          <option value="system">系统默认</option>
          <option value="sans">无衬线</option>
          <option value="mono">等宽</option>
        </select>
      </SettingRow>
      <SettingRow label="自动保存会话" desc="会话内容自动保存到磁盘">
        <label className="toggle-switch"><input type="checkbox" checked={autoSave} onChange={(e) => setAutoSave(e.target.checked)} /><span className="toggle-slider"></span></label>
      </SettingRow>
      <SettingRow label="标签页恢复" desc="重启后自动恢复所有标签和滚动位置">
        <label className="toggle-switch"><input type="checkbox" checked={tabRestore} onChange={(e) => setTabRestore(e.target.checked)} /><span className="toggle-slider"></span></label>
      </SettingRow>
    </div>
  );
}

