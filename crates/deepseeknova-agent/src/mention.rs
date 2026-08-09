//! # @-mention 引用解析
//!
//! 主对话中用 `@agent_name` 引用子代理：`@` 后跟合法 agent 名
//! （`[A-Za-z0-9][A-Za-z0-9_-]*`），前后须为词边界（防 `a@b`、`@coder_x`
//! 误命中）。解析结果按子代理注册表（已知名集合）过滤，供调度方决定派发目标。
//!
//! 纯函数、无 IO，便于单元测试与 CLI/runtime 在入口处复用（主对话
//! @-mention 拦截点由上层接线，本模块提供解析与消歧语义）。

use crate::agent_manifest::is_valid_agent_name;

/// 一个 @ 引用的命中位置。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Mention {
    pub name: String,
    pub start: usize,
    pub end: usize,
}

impl Mention {
    /// 该引用是否属于已知子代理名集合。
    pub fn is_known(&self, known: &[String]) -> bool {
        known.iter().any(|k| k == &self.name)
    }
}

/// 提取文本中全部 `@name` 引用（保持出现顺序，去重保留首次）。
///
/// 规则：
/// - `@` 后紧跟合法 agent 名（首字符字母数字，续字符字母数字/`_`/`-`）；
/// - `@` 前一个字符不得为标识符字符（词边界起始）；名后一个字符也不得为
///   标识符字符——`@coder_x` 只命中 `coder_x` 而不拆出 `coder`；
/// - `@` 在邮箱（`a@b.com`）中间不命中（前字符为标识符）。
pub fn extract_mentions(text: &str) -> Vec<Mention> {
    let bytes = text.as_bytes();
    let mut out: Vec<Mention> = Vec::new();
    let mut i = 0usize;
    while i < bytes.len() {
        if bytes[i] != b'@' {
            i += 1;
            continue;
        }
        // 词边界：@ 前必须是起始或非标识符字符
        let prev_ok = i == 0 || !is_ident_char(bytes[i - 1]);
        if !prev_ok {
            i += 1;
            continue;
        }
        // 扫描名字
        let mut j = i + 1;
        if j < bytes.len() && !bytes[j].is_ascii_alphanumeric() {
            i += 1;
            continue;
        }
        while j < bytes.len() && is_ident_char(bytes[j]) {
            j += 1;
        }
        // 名后必须是结尾或非标识符字符
        if j < bytes.len() && is_ident_char(bytes[j]) {
            i += 1;
            continue;
        }
        let name = &text[i + 1..j];
        // 只取 ASCII 合法名；非 ASCII 会被 is_ascii 校验拒绝
        if !name.is_empty() && is_valid_agent_name(name) && !out.iter().any(|m| m.name == name) {
            out.push(Mention {
                name: name.to_string(),
                start: i,
                end: j,
            });
        }
        i = j;
    }
    out
}

fn is_ident_char(b: u8) -> bool {
    b.is_ascii_alphanumeric() || b == b'_' || b == b'-'
}

/// 首个命中的已知子代理名（无已知引用返回 None）。
pub fn first_known_mention(text: &str, known: &[String]) -> Option<Mention> {
    extract_mentions(text)
        .into_iter()
        .find(|m| m.is_known(known))
}

/// 消歧：已知引用恰好一个 → Some；零个 → None；多个 → Err（调度方需
/// 提示用户一次只引用一个子代理）。
pub fn resolve_mention(text: &str, known: &[String]) -> Result<Option<Mention>, MentionError> {
    let known_mentions: Vec<Mention> = extract_mentions(text)
        .into_iter()
        .filter(|m| m.is_known(known))
        .collect();
    match known_mentions.len() {
        0 => Ok(None),
        1 => Ok(known_mentions.into_iter().next()),
        _ => {
            let names: Vec<String> = known_mentions.into_iter().map(|m| m.name).collect();
            Err(MentionError::Ambiguous(names))
        }
    }
}

/// @-mention 消歧失败。
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum MentionError {
    #[error("ambiguous @-mention: matches multiple sub-agents {0:?} — reference exactly one")]
    Ambiguous(Vec<String>),
}

/// 把 [`MentionError`] 转换为 [`deepseeknova_core::DeepseeknovaError`]。
///
/// orphan rule：impl 放在拥有 `MentionError` 的本 crate。`?` 可直接把
/// `Result<_, MentionError>` 用于返回 `Result<_, DeepseeknovaError>` 的函数。
impl From<MentionError> for deepseeknova_core::DeepseeknovaError {
    fn from(err: MentionError) -> Self {
        deepseeknova_core::DeepseeknovaError::Agent(err.to_string())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn known(names: &[&str]) -> Vec<String> {
        names.iter().map(|s| s.to_string()).collect()
    }

    #[test]
    fn extracts_simple_mention() {
        let m = extract_mentions("@coder finish the refactor");
        assert_eq!(m.len(), 1);
        assert_eq!(m[0].name, "coder");
    }

    #[test]
    fn extracts_multiple_in_order() {
        let m = extract_mentions("@coder then @reviewer check it");
        let names: Vec<&str> = m.iter().map(|x| x.name.as_str()).collect();
        assert_eq!(names, vec!["coder", "reviewer"]);
    }

    #[test]
    fn dedupes_mentions() {
        let m = extract_mentions("@coder @coder again");
        assert_eq!(m.len(), 1);
        assert_eq!(m[0].name, "coder");
    }

    #[test]
    fn does_not_match_email() {
        let m = extract_mentions("email me at a@b.com please");
        assert!(m.is_empty(), "email domain must not be a mention");
    }

    #[test]
    fn does_not_split_longer_name() {
        let m = extract_mentions("use @coder_x now");
        assert_eq!(m.len(), 1);
        assert_eq!(m[0].name, "coder_x");
    }

    #[test]
    fn word_boundary_before_at() {
        // 前有标识符字符 → 不命中
        let m = extract_mentions("acoder");
        assert!(m.is_empty());
    }

    #[test]
    fn mention_at_start_and_after_punct() {
        let m = extract_mentions("(@coder) and [@reviewer]");
        assert_eq!(m.len(), 2);
        assert_eq!(m[0].name, "coder");
        assert_eq!(m[1].name, "reviewer");
    }

    #[test]
    fn invalid_names_ignored() {
        // @ 后跟非字母数字起始（-/_）不算合法名；@1abc 合法（允许数字开头）
        let m = extract_mentions("@-dash @_under @1abc");
        assert_eq!(m.len(), 1);
        assert_eq!(m[0].name, "1abc");
    }

    #[test]
    fn positions_are_byte_offsets() {
        let m = extract_mentions("hi @coder done").pop().unwrap();
        assert_eq!(&"hi @coder done"[m.start..m.end], "@coder");
    }

    #[test]
    fn resolve_single_known() {
        let known = known(&["coder", "reviewer"]);
        let r = resolve_mention("please @reviewer look", &known).unwrap();
        assert_eq!(r.unwrap().name, "reviewer");
    }

    #[test]
    fn resolve_none_known_falls_back() {
        let known = known(&["coder"]);
        let r = resolve_mention("@unknown do it", &known).unwrap();
        assert!(r.is_none());
    }

    #[test]
    fn resolve_ambiguous_errors() {
        let known = known(&["coder", "reviewer"]);
        let err = resolve_mention("@coder and @reviewer both", &known).unwrap_err();
        assert!(matches!(err, MentionError::Ambiguous(_)));
    }

    #[test]
    fn first_known_mention_picks_first_known() {
        let known = known(&["reviewer"]);
        let m = first_known_mention("@coder @reviewer", &known).unwrap();
        assert_eq!(m.name, "reviewer");
    }
}
