import { useState, useEffect, useCallback } from "react";
import { SettingRow } from "./Shared";
import { useI18n, LOCALES, type Locale } from "../../i18n";
import { getConfig, saveSettings, loadSettings } from "../../bridge";

export default function GeneralSettings() {
  const { locale, setLocale, t } = useI18n();
  const [apiKey, setApiKey] = useState("");
  const [baseUrl, setBaseUrl] = useState("https://api.deepseek.com");
  const [defaultModel, setDefaultModel] = useState("deepseek-v4-flash");
  const [fontSize, setFontSize] = useState(13);
  const [fontFamily, setFontFamily] = useState("system");
  const [autoSave, setAutoSave] = useState(true);
  const [tabRestore, setTabRestore] = useState(true);
  const [loading, setLoading] = useState(true);
  const [saving, setSaving] = useState(false);
  const [saveStatus, setSaveStatus] = useState<"idle" | "ok" | "err">("idle");

  // Load config + settings on mount
  useEffect(() => {
    (async () => {
      try {
        // 1. Load TOML config (contains API key, base URL, model)
        try {
          const configJson = await getConfig();
          const config = JSON.parse(configJson);
          const provider = config.providers?.[0];
          if (provider) {
            setBaseUrl(provider.base_url || "https://api.deepseek.com");
            setDefaultModel(provider.model || "deepseek-v4-flash");
            // If api_key is inline in config, show it; if api_key_env, show placeholder
            if (provider.api_key) {
              setApiKey(provider.api_key);
            } else if (provider.api_key_env) {
              setApiKey(`$${provider.api_key_env}`);
            }
          }
          if (config.default_model) {
            setDefaultModel(config.default_model);
          }
        } catch {
          // getConfig may fail if config file doesn't exist yet — that's fine
        }

        // 2. Load UI settings (font size, font family, autoSave, tabRestore)
        try {
          const settings = await loadSettings();
          if (settings.general) {
            if (settings.general.fontSize) setFontSize(settings.general.fontSize);
            if (settings.general.fontFamily) setFontFamily(settings.general.fontFamily);
            if (settings.general.autoSave !== undefined) setAutoSave(settings.general.autoSave);
            if (settings.general.tabRestore !== undefined) setTabRestore(settings.general.tabRestore);
          }
        } catch {
          // No settings file yet — use defaults
        }
      } finally {
        setLoading(false);
      }
    })();
  }, []);

  // Persist a single setting field to the settings file
  const persistSetting = useCallback(async (key: string, value: any) => {
    setSaving(true);
    setSaveStatus("idle");
    try {
      const settings = await loadSettings().catch(() => ({}));
      if (!settings.general) settings.general = {};
      settings.general[key] = value;
      await saveSettings(settings);
      setSaveStatus("ok");
      setTimeout(() => setSaveStatus("idle"), 2000);
    } catch (e) {
      console.error("Failed to save setting:", key, e);
      setSaveStatus("err");
    } finally {
      setSaving(false);
    }
  }, []);

  // Save API key / base URL / model to config TOML via settings bridge
  const persistConfig = useCallback(async (field: string, value: string) => {
    setSaving(true);
    setSaveStatus("idle");
    try {
      const settings = await loadSettings().catch(() => ({}));
      if (!settings.provider) settings.provider = {};
      settings.provider[field] = value;
      await saveSettings(settings);
      setSaveStatus("ok");
      setTimeout(() => setSaveStatus("idle"), 2000);
    } catch (e) {
      console.error("Failed to save config:", field, e);
      setSaveStatus("err");
    } finally {
      setSaving(false);
    }
  }, []);

  if (loading) {
    return <div style={{ padding: 20, fontSize: 11, color: "var(--text-3)" }}>加载配置中…</div>;
  }

  return (
    <div style={{ display: "flex", flexDirection: "column", gap: 0 }}>
      <SettingRow label={t("general.language")} desc={t("general.languageDesc")}>
        <div style={{ display: "flex", gap: 6 }}>
          {LOCALES.map((l) => (
            <button
              key={l.id}
              className={`btn ${locale === l.id ? "btn-primary" : ""}`}
              onClick={() => setLocale(l.id as Locale)}
              style={{ padding: "4px 12px", fontSize: 11 }}
            >
              {l.label}
            </button>
          ))}
        </div>
      </SettingRow>

      <SettingRow label={t("general.apiKey")} desc={t("general.apiKeyDesc")}>
        <input
          className="input"
          type="password"
          value={apiKey}
          onChange={(e) => setApiKey(e.target.value)}
          onBlur={() => persistConfig("api_key", apiKey)}
          placeholder="sk-..."
          style={{ width: 260 }}
        />
      </SettingRow>

      <SettingRow label={t("general.baseUrl")} desc={t("general.baseUrlDesc")}>
        <input
          className="input"
          value={baseUrl}
          onChange={(e) => setBaseUrl(e.target.value)}
          onBlur={() => persistConfig("base_url", baseUrl)}
          style={{ width: 260 }}
        />
      </SettingRow>

      <SettingRow label={t("general.model")} desc={t("general.modelDesc")}>
        <select
          className="input"
          value={defaultModel}
          onChange={(e) => {
            setDefaultModel(e.target.value);
            persistConfig("model", e.target.value);
          }}
          style={{ width: 200 }}
        >
          <option value="deepseek-v4-flash">DeepSeek v4 Flash</option>
          <option value="deepseek-v4-pro">DeepSeek v4 Pro</option>
          <option value="deepseek-coder">DeepSeek Coder</option>
          <option value="deepseek-reasoner">DeepSeek Reasoner R1</option>
        </select>
      </SettingRow>

      <SettingRow label={t("general.fontSize")} desc={`${fontSize}px`}>
        <input
          type="range"
          min="11"
          max="18"
          value={fontSize}
          onChange={(e) => setFontSize(Number(e.target.value))}
          onMouseUp={() => persistSetting("fontSize", fontSize)}
          style={{ width: 200 }}
        />
      </SettingRow>

      <SettingRow label={t("general.fontFamily")} desc="">
        <select
          className="input"
          value={fontFamily}
          onChange={(e) => {
            setFontFamily(e.target.value);
            persistSetting("fontFamily", e.target.value);
          }}
          style={{ width: 200 }}
        >
          <option value="system">System</option>
          <option value="sans">Sans-serif</option>
          <option value="mono">Monospace</option>
        </select>
      </SettingRow>

      <SettingRow label={t("general.autoSave")} desc={t("general.autoSaveDesc")}>
        <label className="toggle-switch">
          <input
            type="checkbox"
            checked={autoSave}
            onChange={(e) => {
              setAutoSave(e.target.checked);
              persistSetting("autoSave", e.target.checked);
            }}
          />
          <span className="toggle-slider"></span>
        </label>
      </SettingRow>

      <SettingRow label={t("general.tabRestore")} desc={t("general.tabRestoreDesc")}>
        <label className="toggle-switch">
          <input
            type="checkbox"
            checked={tabRestore}
            onChange={(e) => {
              setTabRestore(e.target.checked);
              persistSetting("tabRestore", e.target.checked);
            }}
          />
          <span className="toggle-slider"></span>
        </label>
      </SettingRow>

      {saving && (
        <div style={{ fontSize: 10, color: "var(--text-3)", padding: "4px 8px" }}>保存中…</div>
      )}
      {saveStatus === "ok" && (
        <div style={{ fontSize: 10, color: "var(--green)", padding: "4px 8px" }}>✓ 已保存</div>
      )}
      {saveStatus === "err" && (
        <div style={{ fontSize: 10, color: "var(--red)", padding: "4px 8px" }}>保存失败</div>
      )}
    </div>
  );
}
