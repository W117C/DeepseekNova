//! 协议门控装配（`[protocol]` 段）：phase gates + 对抗审查开关。
//! M7b 拆分：从 lib.rs 纯搬移，不修改行为/签名。

use deepseeknova_config::Config;

/// 协议门控装配（协议增强设计 §3.3/§3.4）：`[protocol] enabled=true` 时，
/// 解析 `config.protocol.gates`（`HashMap<String, String>`，值 `hard|soft|off`，
/// 非法值 warn 跳过该项；缺省门名用 `builtin_phase_gates` 内置默认力度表）
/// → `agent::phase_runner::builtin_phase_gates(&levels)` →
/// `Agent::with_protocol_gates`。`enabled=false`（默认）原样返回，Agent
/// 行为零变化（零成本路径，见 phase_runner 文档）。
///
/// 对抗审查（设计 §4.2）：`config.protocol.adversarial_review=true` 时调用
/// `Agent::with_adversarial_review(true)`——开关独立于 `enabled`，E 侧
/// 全包 spawn/写报告，runtime 只传开关。
///
/// `workspace_root` 暂未使用（预留：未来门配置可能含工作区相对路径），
/// 保持签名与装配链其他函数一致。
pub fn attach_protocol_gates(
    agent: deepseeknova_agent::Agent,
    config: &Config,
    _workspace_root: &std::path::Path,
) -> deepseeknova_agent::Agent {
    let mut agent = agent;
    if config.protocol.enabled {
        use deepseeknova_agent::phase_runner::GateLevel;
        use std::str::FromStr;

        let mut levels: std::collections::HashMap<String, GateLevel> =
            std::collections::HashMap::new();
        for (name, raw) in &config.protocol.gates {
            match GateLevel::from_str(raw) {
                Ok(level) => {
                    levels.insert(name.clone(), level);
                }
                Err(e) => {
                    tracing::warn!("protocol gate '{name}' skipped: {e} (config value '{raw}')");
                }
            }
        }
        let gates = deepseeknova_agent::phase_runner::builtin_phase_gates(&levels);
        agent = agent.with_protocol_gates(gates);
    }
    if config.protocol.adversarial_review {
        agent = agent.with_adversarial_review(true);
    }
    agent
}
#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::*;
    use std::sync::Arc;

    /// 协议增强 §3.4：`[protocol] enabled=false`（默认）时 attach_protocol_gates
    /// 原样返回——run 事件流中不出现任何 PhaseTransition（protocol_active =
    /// gates 非空，零成本路径）。
    #[tokio::test]
    async fn protocol_gates_disabled_leaves_agent_unchanged() {
        use deepseeknova_core::Runner;
        use futures::StreamExt;

        let mut config = Config::default();
        config.protocol.enabled = false;
        config.protocol.adversarial_review = false;
        let root = std::path::Path::new("");
        let agent = attach_protocol_gates(
            deepseeknova_agent::Agent::new(Arc::new(EmptyProvider), 2),
            &config,
            root,
        );
        let mut stream = agent
            .run_stream(deepseeknova_core::RunInput {
                prompt: "hi".into(),
                images: Vec::new(),
                model_override: None,
            })
            .await
            .unwrap();
        while let Some(ev) = stream.next().await {
            let ev = ev.unwrap();
            assert!(
                !matches!(
                    ev,
                    deepseeknova_core::runner::RunEvent::PhaseTransition { .. }
                ),
                "protocol disabled must not emit phase events"
            );
        }
    }

    /// 协议增强 §3.4：enabled=true 时门注入（run 事件流出现 PhaseTransition
    /// 事件）；gates 配置解析（hard/soft/off + 非法值 warn 跳过）。
    #[tokio::test]
    async fn protocol_gates_enabled_injects_gates_and_parses_levels() {
        use deepseeknova_core::Runner;
        use futures::StreamExt;
        use std::str::FromStr;

        // GateLevel 解析：合法三值 + 非法值报错（运行时 warn 跳过该项）。
        assert!(matches!(
            deepseeknova_agent::phase_runner::GateLevel::from_str("hard").unwrap(),
            deepseeknova_agent::phase_runner::GateLevel::Hard
        ));
        assert!(matches!(
            deepseeknova_agent::phase_runner::GateLevel::from_str("soft").unwrap(),
            deepseeknova_agent::phase_runner::GateLevel::Soft
        ));
        assert!(matches!(
            deepseeknova_agent::phase_runner::GateLevel::from_str("off").unwrap(),
            deepseeknova_agent::phase_runner::GateLevel::Off
        ));
        assert!(deepseeknova_agent::phase_runner::GateLevel::from_str("bogus").is_err());

        // builtin_phase_gates：缺省力度 + 覆盖 → 4 门。
        let mut levels: std::collections::HashMap<
            String,
            deepseeknova_agent::phase_runner::GateLevel,
        > = std::collections::HashMap::new();
        levels.insert(
            "verify-evidence".to_string(),
            deepseeknova_agent::phase_runner::GateLevel::from_str("hard").unwrap(),
        );
        let gates = deepseeknova_agent::phase_runner::builtin_phase_gates(&levels);
        assert_eq!(gates.len(), 4, "four builtin gates");

        // enabled=true（含一个非法 gate 值）→ run 产 PhaseTransition 事件。
        let mut config = Config::default();
        config.protocol.enabled = true;
        config
            .protocol
            .gates
            .insert("verify-evidence".to_string(), "hard".to_string());
        config
            .protocol
            .gates
            .insert("drift-detection".to_string(), "off".to_string());
        config
            .protocol
            .gates
            .insert("unknown-gate".to_string(), "bogus".to_string());
        let agent = attach_protocol_gates(
            deepseeknova_agent::Agent::new(Arc::new(EmptyProvider), 2),
            &config,
            std::path::Path::new(""),
        );
        let mut stream = agent
            .run_stream(deepseeknova_core::RunInput {
                prompt: "hi".into(),
                images: Vec::new(),
                model_override: None,
            })
            .await
            .unwrap();
        let mut transitions = 0usize;
        while let Some(ev) = stream.next().await {
            if matches!(
                ev.unwrap(),
                deepseeknova_core::runner::RunEvent::PhaseTransition { .. }
            ) {
                transitions += 1;
            }
        }
        assert!(
            transitions >= 1,
            "protocol enabled must emit at least one phase transition, got {transitions}"
        );
    }

    /// 对抗审查开关独立于 enabled：`adversarial_review=true` 时即使
    /// enabled=false 也走 with_adversarial_review 装配路径（run 正常结束，
    /// 不 panic）；门注入保持零成本。
    #[tokio::test]
    async fn protocol_gates_adversarial_review_flag_wires_independent_of_enabled() {
        use deepseeknova_core::Runner;
        use futures::StreamExt;

        let mut config = Config::default();
        config.protocol.enabled = false;
        config.protocol.adversarial_review = true;
        let agent = attach_protocol_gates(
            deepseeknova_agent::Agent::new(Arc::new(stub_provider()), 2),
            &config,
            std::path::Path::new(""),
        );
        let mut stream = agent
            .run_stream(deepseeknova_core::RunInput {
                prompt: "hi".into(),
                images: Vec::new(),
                model_override: None,
            })
            .await
            .unwrap();
        while let Some(ev) = stream.next().await {
            ev.unwrap();
        }
    }
}
