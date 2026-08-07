//! 测光·评分卡：从 serve 落盘 JSON 解析六维光度表。

use serde_json::Value;
use std::path::Path;

/// 六维光度表维度（顺序即展示顺序）。
pub const SCORECARD_DIMS: [&str; 6] = ["治理", "验证", "反思", "审查", "协议", "综合"];

#[derive(Debug, Clone, PartialEq, Default)]
pub struct Scorecard {
    pub rows: Vec<ScorecardRow>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct ScorecardRow {
    pub dim: String,
    pub score: f64,
}

impl Scorecard {
    /// 解析 serve 落盘 JSON：接受 `{"scores": {...}}`、`{"scorecard": {...}}`
    /// 或直接对象三种形态；六维中至少一维可解析才算有效。
    pub fn parse_json(text: &str) -> Option<Self> {
        let value: Value = serde_json::from_str(text).ok()?;
        let obj = value.as_object()?;
        let scores = obj
            .get("scores")
            .or_else(|| obj.get("scorecard"))
            .unwrap_or(&value);
        let map = scores.as_object()?;
        let rows: Vec<ScorecardRow> = SCORECARD_DIMS
            .iter()
            .filter_map(|dim| {
                let score = map.get(*dim)?.as_f64()?;
                Some(ScorecardRow {
                    dim: dim.to_string(),
                    score: score.clamp(0.0, 100.0),
                })
            })
            .collect();
        if rows.is_empty() {
            None
        } else {
            Some(Scorecard { rows })
        }
    }

    /// 读取目录中修改时间最新的 JSON 评分卡。
    pub fn latest_from_dir(dir: &Path) -> Option<Self> {
        let entries = std::fs::read_dir(dir).ok()?;
        let mut best: Option<(std::time::SystemTime, std::path::PathBuf)> = None;
        for entry in entries.flatten() {
            let path = entry.path();
            if path.extension().and_then(|e| e.to_str()) != Some("json") {
                continue;
            }
            let Ok(meta) = entry.metadata() else {
                continue;
            };
            let Ok(modified) = meta.modified() else {
                continue;
            };
            if best.as_ref().is_none_or(|(t, _)| modified > *t) {
                best = Some((modified, path));
            }
        }
        let (_, path) = best?;
        let text = std::fs::read_to_string(path).ok()?;
        Self::parse_json(&text)
    }
}

/// 十格测光横条：`██████░░░░`（score/10 取整，0..100 钳制）。
pub fn photometry_bar(score: f64) -> String {
    let filled = (score.clamp(0.0, 100.0) / 10.0).round() as usize;
    format!("{}{}", "█".repeat(filled), "░".repeat(10 - filled))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_scores_object_in_dim_order() {
        let sc = Scorecard::parse_json(
            r#"{"scores":{"治理":92.3,"验证":94.7,"反思":88.1,"审查":90.5,"协议":96.2,"综合":92.0}}"#,
        )
        .unwrap();
        assert_eq!(
            sc.rows.iter().map(|r| r.dim.as_str()).collect::<Vec<_>>(),
            vec!["治理", "验证", "反思", "审查", "协议", "综合"]
        );
        assert_eq!(sc.rows[5].score, 92.0);
    }

    #[test]
    fn accepts_scorecard_array_shape_and_clamps() {
        let sc = Scorecard::parse_json(r#"{"scorecard":{"治理":120,"综合":-5}}"#).unwrap();
        assert_eq!(sc.rows[0].score, 100.0);
        assert_eq!(sc.rows[1].score, 0.0);
    }

    #[test]
    fn rejects_malformed_or_empty() {
        assert!(Scorecard::parse_json("not json").is_none());
        assert!(Scorecard::parse_json(r#"{"scores":{"nope":1}}"#).is_none());
        assert!(Scorecard::parse_json("null").is_none());
    }

    #[test]
    fn latest_from_dir_picks_newest_json() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("old.json"), r#"{"scores":{"治理":10}}"#).unwrap();
        std::thread::sleep(std::time::Duration::from_millis(20));
        std::fs::write(dir.path().join("new.json"), r#"{"scores":{"治理":90}}"#).unwrap();
        let sc = Scorecard::latest_from_dir(dir.path()).unwrap();
        assert_eq!(sc.rows[0].score, 90.0);
    }

    #[test]
    fn latest_from_dir_empty_returns_none() {
        let dir = tempfile::tempdir().unwrap();
        assert!(Scorecard::latest_from_dir(dir.path()).is_none());
        assert!(Scorecard::latest_from_dir(&dir.path().join("missing")).is_none());
    }

    #[test]
    fn photometry_bar_renders_ten_cells() {
        assert_eq!(photometry_bar(92.3), "█████████░");
        assert_eq!(photometry_bar(50.0), "█████░░░░░");
        assert_eq!(photometry_bar(0.0), "░░░░░░░░░░");
    }
}
