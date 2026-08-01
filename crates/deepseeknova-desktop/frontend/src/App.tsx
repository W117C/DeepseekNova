/**
 * App.tsx — 应用入口
 * 负责初始化数据 + 主题 + i18n，渲染 AppChrome
 */

import { useEffect, useState, useCallback } from "react";
import { useStore } from "./store";
import { listSkills, getCapabilities } from "./bridge";
import { initTheme } from "./store/theme";
import { I18nContext, translate, type Locale } from "./i18n";
import AppChrome from "./components/AppChrome";

export default function App() {
  const setSkills = useStore((s) => s.setSkills);
  const setCapabilities = useStore((s) => s.setCapabilities);

  const [locale, setLocaleState] = useState<Locale>(() => {
    return (localStorage.getItem("dsn-locale") as Locale) || "zh-CN";
  });

  const setLocale = useCallback((l: Locale) => {
    setLocaleState(l);
    localStorage.setItem("dsn-locale", l);
  }, []);

  const t = useCallback(
    (key: string, params?: Record<string, string | number>) => translate(locale, key, params),
    [locale]
  );

  useEffect(() => {
    initTheme();
    getCapabilities().then(setCapabilities).catch(console.error);
    listSkills().then(setSkills).catch(console.error);
  }, [setCapabilities, setSkills]);

  return (
    <I18nContext.Provider value={{ locale, setLocale, t }}>
      <AppChrome />
    </I18nContext.Provider>
  );
}
