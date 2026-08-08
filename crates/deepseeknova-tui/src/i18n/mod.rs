//! 双语 i18n 框架：英文默认 + 中文可选，零外部依赖。
//!
//! # 结构
//!
//! - [`Lang`]：语言枚举（`En` 默认 / `Zh` 可选），`code()` 返回 `"en"`/`"zh"`。
//! - [`Key`]：全部用户可见文案的稳定键（定义见 `keys` 子模块）。
//!   每语言一个静态表：`en()` 兜底、`zh()` 可选（缺键回退英文，fail-safe）。
//! - [`Tr`]：轻量翻译器，携带当前语言，提供 `t`（静态文案）与 `t_args`
//!   （`{name}` 命名占位符插值）。
//!
//! # 语言选择
//!
//! `TuiRunner::with_lang` 编程式注入（CLI 后续可从配置项接线），缺省读取
//! `DEEPSEEKNOVA_LANG` 环境变量（`zh`/`zh-cn`/`cn`/`中文` → 中文，其余/缺省 → 英文）。
//!
//! # 桌面端复用（Tauri 壳 P2 契约）
//!
//! 本词表结构设计为与桌面前端共享：
//! - **键名即契约**：`Key` 变体名是稳定标识符，前端镜像同一键集（枚举或
//!   共享 JSON），按 `lang_code` 取词；新增文案先加键，再补两语言值。
//! - **占位符规范**：结构化文案统一 `{name}` 命名占位符（如
//!   `Reached step limit ({n})`），前端用同一插值约定，避免按位置拼接。
//! - **fail-safe 语义**：目标语言缺键时回退英文，两端一致；前端不应为
//!   缺键抛错。
//! - P2 壳接入时保持键名与占位符名不变（这是跨端契约，改键即破坏同步）。

mod keys;

pub use keys::{Key, ALL_KEYS};

/// 支持的语言。默认英文。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Hash)]
pub enum Lang {
    /// 英文（默认）。
    #[default]
    En,
    /// 中文（可选）。
    Zh,
}

impl Lang {
    /// 全部语言（遍历用）。
    pub const ALL: [Lang; 2] = [Lang::En, Lang::Zh];

    /// 从 `DEEPSEEKNOVA_LANG` 环境变量解析（未知/缺省回退英文）。
    pub fn from_env() -> Lang {
        let raw = std::env::var("DEEPSEEKNOVA_LANG").unwrap_or_default();
        let norm = raw.trim().to_ascii_lowercase().replace('-', "_");
        match norm.as_str() {
            "zh" | "zh_cn" | "cn" | "chinese" | "中文" => Lang::Zh,
            _ => Lang::En,
        }
    }

    /// 语言代码（前端取词用，`en` / `zh`）。
    pub fn code(self) -> &'static str {
        match self {
            Lang::En => "en",
            Lang::Zh => "zh",
        }
    }
}

/// 轻量翻译器：携带当前语言，按 [`Key`] 查表。
///
/// `Copy` 且零分配（除 `t_args` 插值外）；`Default` 为英文，测试可直接
/// `Tr::new(Lang::Zh)` 固定断言语言。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Tr {
    lang: Lang,
}

impl Tr {
    /// 指定语言构造。
    pub fn new(lang: Lang) -> Self {
        Self { lang }
    }

    /// 当前语言。
    pub fn lang(self) -> Lang {
        self.lang
    }

    /// 静态文案查表（无参数）。
    pub fn t(self, key: Key) -> &'static str {
        key.tr(self.lang)
    }

    /// 结构化文案：`{name}` 命名占位符插值。
    ///
    /// 示例：`Tr::new(Lang::En).t_args(Key::PauseMaxSteps, &[("n", "10"))]`
    /// → `"Reached step limit (10), task incomplete"`。
    pub fn t_args(self, key: Key, args: &[(&str, &str)]) -> String {
        interpolate(self.t(key), args)
    }
}

/// `{name}` → 值的简单插值（零依赖；同名占位符全部替换）。
pub fn interpolate(template: &str, args: &[(&str, &str)]) -> String {
    let mut out = template.to_string();
    for (name, value) in args {
        out = out.replace(&format!("{{{name}}}"), value);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn lang_from_env_parses_aliases() {
        // 归一化：大小写/连字符/下划线/中文别名 → Zh，其余/缺省 → En。
        // 仅本测试读写 DEEPSEEKNOVA_LANG，无并发冲突。
        for (val, expect) in [
            ("zh", Lang::Zh),
            ("zh-CN", Lang::Zh),
            ("zh_cn", Lang::Zh),
            ("cn", Lang::Zh),
            ("chinese", Lang::Zh),
            ("中文", Lang::Zh),
            ("", Lang::En),
            ("en", Lang::En),
            ("fr", Lang::En),
        ] {
            std::env::set_var("DEEPSEEKNOVA_LANG", val);
            assert_eq!(Lang::from_env(), expect, "DEEPSEEKNOVA_LANG={val:?}");
        }
        std::env::remove_var("DEEPSEEKNOVA_LANG");
        assert_eq!(Lang::from_env(), Lang::En, "缺省回退英文");
        assert_eq!(Lang::En.code(), "en");
        assert_eq!(Lang::Zh.code(), "zh");
    }

    #[test]
    fn language_switch_changes_value() {
        let tr_en = Tr::new(Lang::En);
        let tr_zh = Tr::new(Lang::Zh);
        assert_eq!(tr_en.t(Key::WelcomeHelp), "Type /help to see all commands");
        assert_eq!(tr_zh.t(Key::WelcomeHelp), "输入 /help 查看全部命令");
        assert_eq!(tr_zh.t(Key::PressEscAgain), "再按 Esc 退出");
        assert_eq!(tr_en.t(Key::PressEscAgain), "Press Esc again to exit");
        assert_ne!(tr_en.t(Key::FoldedTool), tr_zh.t(Key::FoldedTool));
        assert_eq!(tr_en.lang(), Lang::En);
        assert_eq!(tr_zh.lang(), Lang::Zh);
    }

    #[test]
    fn interpolation_substitutes_named_placeholders() {
        let tr = Tr::new(Lang::En);
        assert_eq!(
            tr.t_args(Key::PauseMaxSteps, &[("n", "10")]),
            "Reached step limit (10), task incomplete"
        );
        let tr_zh = Tr::new(Lang::Zh);
        assert_eq!(
            tr_zh.t_args(Key::PauseMaxSteps, &[("n", "10")]),
            "已达步骤上限（10），任务未完成"
        );
        // 多个占位符 + 重复替换。
        assert_eq!(
            tr.t_args(Key::ResumeDone, &[("target", "abc"), ("n", "3")]),
            "Resumed 'abc' — 3 messages (in conversation pane, scroll/fold)"
        );
        // 同一占位符多处出现全部替换。
        assert_eq!(
            tr.t_args(
                Key::HelpPager,
                &[("start", "1"), ("end", "5"), ("total", "9")]
            ),
            " · 1-5/9 lines · j/k scroll · Esc close"
        );
    }

    #[test]
    fn missing_key_falls_back_to_english() {
        // 技术性文案在中文模式缺词表键 → fail-safe 回退英文。
        let tr_zh = Tr::new(Lang::Zh);
        assert_eq!(tr_zh.t(Key::ModelCommandsHeader), "Model commands:");
        assert_eq!(tr_zh.t(Key::ThinkingToggle), "thinking {from} → {to}");
        // 与英文表一致（缺键不返回空串/不 panic）。
        assert_eq!(tr_zh.t(Key::CtxUsage), Tr::new(Lang::En).t(Key::CtxUsage));
        // 有中文值的键在英文模式也不受影响。
        assert_eq!(
            Tr::new(Lang::En).t(Key::WelcomeHelp),
            "Type /help to see all commands"
        );
    }

    #[test]
    fn key_en_values_are_nonempty_smoke() {
        // en() 由穷举 match 保证编译期完整性；这里抽查关键域的兜底值非空。
        for k in [
            Key::ThinkingVerbs,
            Key::CmdHelpDesc,
            Key::PauseMaxSteps,
            Key::UnknownCommand,
            Key::ThemeUnknownFallback,
            Key::KeymapReserved,
            Key::ReservedCtrlBackslash,
            Key::ScorecardDimComposite,
        ] {
            assert!(!k.en().is_empty(), "en 值不可空: {k:?}");
        }
    }

    #[test]
    fn all_keys_bilingual_values_nonempty() {
        // 穷举：每个键的英文值非空；中文值（显式返回 None 的技术性键除外）
        // 非空。新增词条既要在 en()/zh() 的 match 补齐（编译器强制），
        // 也要加入 ALL_KEYS 表，否则此测试漏检。
        assert_eq!(ALL_KEYS.len(), 257, "ALL_KEYS 与枚举变体数一致");
        let tr_en = Tr::new(Lang::En);
        let tr_zh = Tr::new(Lang::Zh);
        for k in ALL_KEYS {
            let en = tr_en.t(*k);
            assert!(!en.is_empty(), "en 值不可空: {k:?}");
            let zh = tr_zh.t(*k);
            assert!(
                !zh.is_empty(),
                "中文回退后不可为空串（应回退英文或给出中文值）: {k:?}"
            );
            // 显式 None 的技术性键：中文与英文一致（回退）。
            if k.zh().is_none() {
                assert_eq!(zh, en, "zh()=None 的键中文模式应回退英文: {k:?}");
            }
        }
    }
}
