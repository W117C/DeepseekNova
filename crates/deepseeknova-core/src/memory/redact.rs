//! # Secret Redaction
//!
//! 在把任何内容写入持久记忆库前调用，抹除常见密钥/token/私钥，
//! 避免 `.env`、报错信息里的凭据被无确认写入。宁可误伤，不可泄露。

use regex::Regex;
use std::sync::OnceLock;

/// 脱敏占位符。
const MASK: &str = "[REDACTED]";

struct Patterns {
    kv: Regex,
    aws: Regex,
    pem: Regex,
    bearer: Regex,
}

// 静态常量正则：唯一可能的失败是编译期笔误，用 expect 立即暴露；
// core 禁用 unwrap/expect，故在此函数局部放行。
#[allow(clippy::expect_used)]
fn patterns() -> &'static Patterns {
    static P: OnceLock<Patterns> = OnceLock::new();
    P.get_or_init(|| Patterns {
        // key/secret/token/password = <值> 或 : <值>；允许 GITHUB_TOKEN/MY_API_KEY 等前缀名，
        // 值取任意非空白串（宁可误伤，不可泄露）。
        kv: Regex::new(
            r#"(?i)\b[a-z0-9_-]*(api[_-]?key|secret|token|password|passwd|access[_-]?key)\b\s*[:=]\s*\S{8,}"#,
        )
        .expect("kv regex"),
        // AWS Access Key ID
        aws: Regex::new(r"\bAKIA[0-9A-Z]{16}\b").expect("aws regex"),
        // PEM 私钥块头
        pem: Regex::new(r"-----BEGIN [A-Z ]*PRIVATE KEY-----").expect("pem regex"),
        // Authorization: Bearer <token>
        bearer: Regex::new(r"(?i)bearer\s+[A-Za-z0-9._\-]{12,}").expect("bearer regex"),
    })
}

/// 返回脱敏后的字符串。无命中时返回等价内容。
pub fn redact(input: &str) -> String {
    let p = patterns();
    let s =
        p.kv.replace_all(input, |c: &regex::Captures| format!("{}=[REDACTED]", &c[1]));
    let s = p.aws.replace_all(&s, MASK);
    let s = p.pem.replace_all(&s, MASK);
    let s = p.bearer.replace_all(&s, "Bearer [REDACTED]");
    s.into_owned()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn masks_api_key_kv() {
        let out = redact("export API_KEY=sk-ABCD1234efgh5678");
        assert!(!out.contains("sk-ABCD1234efgh5678"), "got: {out}");
        assert!(out.contains("[REDACTED]"));
    }

    #[test]
    fn masks_aws_and_pem_and_bearer() {
        assert!(redact("id AKIAIOSFODNN7EXAMPLE here").contains("[REDACTED]"));
        assert!(redact("-----BEGIN RSA PRIVATE KEY-----").contains("[REDACTED]"));
        assert!(redact("Authorization: Bearer abcdefghijklmnop").contains("[REDACTED]"));
    }

    #[test]
    fn masks_prefixed_env_style_names() {
        // 回归：带前缀的 .env 风格名必须命中（审查发现的假阴性）。
        for s in [
            "GITHUB_TOKEN=ghp_abcdefghij123456",
            "MY_API_KEY=sk-abcdef12345678",
            "access_token=abcdefgh12345678",
            "password: \"p@ssw0rd123!\"",
        ] {
            let out = redact(s);
            assert!(out.contains("[REDACTED]"), "must redact: {s} -> {out}");
        }
    }

    #[test]
    fn leaves_ordinary_text_untouched() {
        let text = "fn main() { println!(\"hello world\"); }";
        assert_eq!(redact(text), text);
    }

    #[test]
    fn leaves_short_values_untouched() {
        // 太短的赋值不视为密钥，避免误伤（如 x = 1）
        let text = "let x = 1;";
        assert_eq!(redact(text), text);
    }
}
