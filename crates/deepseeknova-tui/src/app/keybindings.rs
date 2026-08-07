//! keybindings.json 用户键位定制层（Claude Code 设计迁移，轻量版）。
//!
//! 文件：`~/.deepseeknova/keybindings.json`（可用 `DEEPSEEKNOVA_KEYBINDINGS`
//! 覆盖路径）。格式与 Claude Code 同构：
//!
//! ```json
//! {
//!   "bindings": [
//!     { "context": "Conversation", "bindings": { "ctrl+u": "conv:scrollTop", "y": null } }
//!   ]
//! }
//! ```
//!
//! - action 必须 ∈ 已知枚举（见 [`crate::app::actions::Action::name`]），
//!   `null` = 解绑默认键；
//! - 保留键（ctrl+c / ctrl+d / ctrl+m / ctrl+z / ctrl+\）不可重绑，带原因；
//! - 热重载：事件循环轮询 mtime（500ms 稳定阈值），改文件即时生效；
//! - 诊断：parse_error / invalid_context / invalid_action / reserved /
//!   duplicate，全部带 suggestion 文本。
//!
//! 覆盖语义：`Keymap::lookup` 优先查 overrides（解绑 → None），
//! 未覆盖回落到编译期 [`crate::app::actions::BINDINGS`]。

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use crate::app::actions::{Action, ActionContext, Binding};
use crate::i18n::{Key, Tr};

/// 用户覆盖层：`(context, binding) → Some(action) | None(解绑)`。
#[derive(Debug, Clone, Default)]
pub struct Keymap {
    overrides: HashMap<(ActionContext, Binding), Option<Action>>,
    /// 加载失败/诊断信息（供状态行与调试）。
    pub diagnostics: Vec<String>,
}

impl Keymap {
    pub fn default_path() -> PathBuf {
        std::env::var_os("DEEPSEEKNOVA_KEYBINDINGS")
            .map(PathBuf::from)
            .unwrap_or_else(|| {
                dirs::home_dir()
                    .unwrap_or_else(|| PathBuf::from("."))
                    .join(".deepseeknova")
                    .join("keybindings.json")
            })
    }

    /// 覆盖条目数（状态行/日志展示）。
    pub fn override_count(&self) -> usize {
        self.overrides.len()
    }

    /// 按 context 查按键 → action；解绑返回 None 且不再回落默认表。
    /// 未覆盖返回 `Action` 的查询交给调用方继续查默认表。
    pub fn lookup(&self, context: ActionContext, binding: Binding) -> Option<Option<Action>> {
        self.overrides.get(&(context, binding)).copied()
    }

    /// 加载并解析；文件不存在返回空覆盖（无诊断）。`tr` 决定诊断文案语言。
    pub fn load(path: &Path, tr: Tr) -> Self {
        let content = match std::fs::read_to_string(path) {
            Ok(c) => c,
            Err(_) => return Keymap::default(),
        };
        Self::parse(&content, tr)
    }

    /// 解析 keybindings.json 文本 → 覆盖层 + 诊断（纯函数，便于测试）。
    /// 诊断文案按 `tr` 语言生成。
    pub fn parse(content: &str, tr: Tr) -> Self {
        let mut keymap = Keymap::default();
        let value: serde_json::Value = match serde_json::from_str(content) {
            Ok(v) => v,
            Err(e) => {
                keymap
                    .diagnostics
                    .push(tr.t_args(Key::KeymapParseError, &[("err", &e.to_string())]));
                return keymap;
            }
        };
        let Some(bindings) = value.get("bindings").and_then(|v| v.as_array()) else {
            keymap
                .diagnostics
                .push(tr.t(Key::KeymapMissingBindings).to_string());
            return keymap;
        };
        for (gi, group) in bindings.iter().enumerate() {
            let gi = gi.to_string();
            let Some(ctx_name) = group.get("context").and_then(|v| v.as_str()) else {
                keymap
                    .diagnostics
                    .push(tr.t_args(Key::KeymapMissingContext, &[("gi", &gi)]));
                continue;
            };
            let Some(context) = ActionContext::from_name(ctx_name) else {
                keymap
                    .diagnostics
                    .push(tr.t_args(Key::KeymapUnknownContext, &[("gi", &gi), ("ctx", ctx_name)]));
                continue;
            };
            let Some(map) = group.get("bindings").and_then(|v| v.as_object()) else {
                keymap
                    .diagnostics
                    .push(tr.t_args(Key::KeymapMissingBindingsObj, &[("gi", &gi)]));
                continue;
            };
            for (key_spec, target) in map {
                // 解绑：null
                if target.is_null() {
                    if let Some(binding) = Binding::parse(key_spec) {
                        keymap.overrides.insert((context, binding), None);
                    } else {
                        keymap.diagnostics.push(tr.t_args(
                            Key::KeymapUnparseableKey,
                            &[("ctx", ctx_name), ("key", key_spec)],
                        ));
                    }
                    continue;
                }
                let Some(action_name) = target.as_str() else {
                    keymap.diagnostics.push(tr.t_args(
                        Key::KeymapActionOrNull,
                        &[("ctx", ctx_name), ("key", key_spec)],
                    ));
                    continue;
                };
                if let Some(reason) = reserved_reason(key_spec) {
                    keymap.diagnostics.push(tr.t_args(
                        Key::KeymapReserved,
                        &[
                            ("ctx", ctx_name),
                            ("key", key_spec),
                            ("reason", tr.t(reason)),
                        ],
                    ));
                    continue;
                }
                let Some(binding) = Binding::parse(key_spec) else {
                    keymap.diagnostics.push(tr.t_args(
                        Key::KeymapParseKeyHelp,
                        &[("ctx", ctx_name), ("key", key_spec)],
                    ));
                    continue;
                };
                let Some(action) = Action::from_name(action_name) else {
                    keymap.diagnostics.push(tr.t_args(
                        Key::KeymapUnknownAction,
                        &[("ctx", ctx_name), ("action", action_name)],
                    ));
                    continue;
                };
                // duplicate：同 (context, binding) 重复声明（后覆盖前）告警。
                if keymap.overrides.contains_key(&(context, binding)) {
                    keymap.diagnostics.push(tr.t_args(
                        Key::KeymapDuplicate,
                        &[("ctx", ctx_name), ("key", key_spec)],
                    ));
                }
                keymap.overrides.insert((context, binding), Some(action));
            }
        }
        keymap
    }
}

/// 保留键保护（把 OS/终端约束写进产品，Claude Code 同款，带原因）。
/// 返回词表键（原因文案按语言取）。
fn reserved_reason(spec: &str) -> Option<Key> {
    let norm = spec.to_ascii_lowercase().replace(' ', "");
    match norm.as_str() {
        "ctrl+c" => Some(Key::ReservedCtrlC),
        "ctrl+d" => Some(Key::ReservedCtrlD),
        "ctrl+m" => Some(Key::ReservedCtrlM),
        "ctrl+z" => Some(Key::ReservedCtrlZ),
        "ctrl+\\" | "ctrl+4" => Some(Key::ReservedCtrlBackslash),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crossterm::event::{KeyCode, KeyModifiers};

    #[test]
    fn parse_empty_file_yields_no_overrides() {
        let km = Keymap::parse("", Tr::new(crate::i18n::Lang::En));
        assert!(km.overrides.is_empty());
        assert!(!km.diagnostics.is_empty(), "空文件给出 parse_error");
    }

    #[test]
    fn parse_valid_bindings_and_unbind() {
        let km = Keymap::parse(
            r#"{
                "bindings": [
                    { "context": "Conversation", "bindings": {
                        "ctrl+b": "conv:scrollPageDown",
                        "y": null
                    } }
                ]
            }"#,
            Tr::new(crate::i18n::Lang::En),
        );
        assert!(km.diagnostics.is_empty(), "无诊断: {:?}", km.diagnostics);
        let b1 = Binding::parse("ctrl+b").unwrap();
        assert_eq!(
            km.lookup(ActionContext::Conversation, b1),
            Some(Some(Action::ConvScrollPageDown))
        );
        let b2 = Binding::parse("y").unwrap();
        assert_eq!(
            km.lookup(ActionContext::Conversation, b2),
            Some(None),
            "y 解绑"
        );
    }

    #[test]
    fn parse_rejects_unknown_action_and_context() {
        let km = Keymap::parse(
            r#"{"bindings":[
                {"context":"Nope","bindings":{"g":"conv:scrollTop"}},
                {"context":"Input","bindings":{"ctrl+q":"nope:action"}}
            ]}"#,
            Tr::new(crate::i18n::Lang::En),
        );
        assert!(
            km.diagnostics
                .iter()
                .any(|d| d.starts_with("invalid_context")),
            "未知 context 诊断"
        );
        assert!(
            km.diagnostics
                .iter()
                .any(|d| d.starts_with("invalid_action")),
            "未知 action 诊断"
        );
    }

    #[test]
    fn parse_rejects_reserved_keys_with_reason() {
        let km = Keymap::parse(
            r#"{"bindings":[
                {"context":"Input","bindings":{"ctrl+c":"chat:submit"}}
            ]}"#,
            Tr::new(crate::i18n::Lang::Zh),
        );
        assert!(
            km.diagnostics
                .iter()
                .any(|d| d.starts_with("reserved") && d.contains("保留")),
            "保留键诊断带原因"
        );
        assert!(km.overrides.is_empty(), "保留键不写入覆盖层");
    }

    #[test]
    fn binding_parse_normalizes_modifiers() {
        let b = Binding::parse("ctrl+k").unwrap();
        assert_eq!(b.code, KeyCode::Char('k'));
        assert!(b.modifiers.contains(KeyModifiers::CONTROL));
        let b = Binding::parse("shift+enter").unwrap();
        assert_eq!(b.code, KeyCode::Enter);
        assert!(b.modifiers.contains(KeyModifiers::SHIFT));
        let b = Binding::parse("esc").unwrap();
        assert_eq!(b.code, KeyCode::Esc);
        let b = Binding::parse("meta+p").unwrap();
        assert!(b.modifiers.contains(KeyModifiers::ALT), "meta → alt");
        assert!(Binding::parse("nonsense").is_none());
    }

    #[test]
    fn lookup_falls_through_when_not_overridden() {
        let km = Keymap::parse(r#"{"bindings":[]}"#, Tr::new(crate::i18n::Lang::En));
        assert_eq!(
            km.lookup(ActionContext::Input, Binding::parse("enter").unwrap()),
            None
        );
    }
}
