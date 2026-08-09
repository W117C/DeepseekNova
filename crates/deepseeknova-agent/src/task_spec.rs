//! # TaskSpec — 结构化委派任务书
//!
//! 借鉴 harness `create-agent` 的 task/inputs/RULES 要素：任务文本支持
//! `${{ inputs.<name> }}` 占位符参数化，声明式输入契约（required / default /
//! 类型校验），约束区（RULES）按任务注入。纯编译期资产，不读文件系统。
//!
//! 设计依据：`docs/superpowers/specs/2026-08-04-task-spec-design.md`。
//! 两条消费路径（`DelegateEngine` 模型自主 spawn、`SubAgentRunner` 显式分发）
//! 各自决定 inputs 来源与渲染结果的注入位置，见各自模块文档。

use std::collections::HashMap;
use std::fmt;

/// 委派任务书：可参数化、带约束的任务定义。
#[derive(Debug, Clone)]
pub struct TaskSpec {
    pub name: String,
    /// 任务指令主体，支持 `${{ inputs.<name> }}` 占位符。
    pub task: String,
    /// 约束区条目，渲染后追加为 `## RULES` 块。软约束（提示区），
    /// 工具白名单（[`Self::tools`]）才是硬限制。
    pub rules: Vec<String>,
    /// 参数化输入声明。
    pub inputs: Vec<InputSpec>,
    /// 工具 schema 名白名单。
    pub tools: Vec<String>,
    /// 执行步数上限。
    pub max_steps: usize,
}

impl TaskSpec {
    /// 无参数化任务的便捷构造（task/rules/inputs 全空）。
    pub fn simple(
        name: impl Into<String>,
        task: impl Into<String>,
        tools: Vec<String>,
        max_steps: usize,
    ) -> Self {
        Self {
            name: name.into(),
            task: task.into(),
            rules: Vec::new(),
            inputs: Vec::new(),
            tools,
            max_steps,
        }
    }

    /// 渲染任务书：校验并替换占位符。纯函数、无 IO、幂等。
    ///
    /// 校验顺序（先快后慢，错误信息含输入名）：
    /// 1. task 占位符引用未声明的输入 → [`TaskSpecError::UnknownInput`]
    ///    （防拼写错误）；
    /// 2. `required` 输入缺值且无 default → [`TaskSpecError::MissingRequired`]；
    /// 3. 提供值（values 或 default）类型非法 → [`TaskSpecError::InvalidType`]。
    ///
    /// values 中未声明的多余键被忽略：调用方可复用同一 values map，
    /// config 传值只对已声明的输入生效。
    pub fn render(&self, values: &InputValues) -> Result<RenderedTask, TaskSpecError> {
        // 1. 收集 task 中引用的输入名，未声明即报错。
        for name in referenced_inputs(&self.task) {
            if !self.inputs.iter().any(|i| i.name == name) {
                return Err(TaskSpecError::UnknownInput(name));
            }
        }

        // 2. 逐输入求值 + 3. 类型校验。值只校验不转换，始终以 String 传递。
        let mut resolved: HashMap<String, String> = HashMap::new();
        for spec in &self.inputs {
            let value = match values.0.get(&spec.name) {
                Some(v) => Some(v.clone()),
                None => spec.default.clone(),
            };
            match &value {
                Some(v) => {
                    if let Err(e) = check_type(&spec.ty, v) {
                        return Err(TaskSpecError::InvalidType(spec.name.clone(), e));
                    }
                    resolved.insert(spec.name.clone(), v.clone());
                }
                None => {
                    if spec.required {
                        return Err(TaskSpecError::MissingRequired(spec.name.clone()));
                    }
                    resolved.insert(spec.name.clone(), String::new());
                }
            }
        }

        let mut task = self.task.clone();
        for (name, value) in &resolved {
            task = task.replace(&format!("${{{{ inputs.{name} }}}}"), value);
        }

        let rules = if self.rules.is_empty() {
            String::new()
        } else {
            let mut out = String::from("## RULES\n");
            for rule in &self.rules {
                out.push_str("- ");
                out.push_str(rule);
                out.push('\n');
            }
            out
        };

        Ok(RenderedTask { task, rules })
    }
}

/// 输入声明。
#[derive(Debug, Clone)]
pub struct InputSpec {
    pub name: String,
    pub ty: InputType,
    pub required: bool,
    /// 缺值时兜底。`required=false` 且无 default 时以空串填充。
    pub default: Option<String>,
}

/// 输入值类型。只做合法性校验，不转换类型。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum InputType {
    String,
    Number,
    Boolean,
}

/// 运行时提供的参数值。String 统一承载，渲染时按 [`InputSpec::ty`] 校验。
#[derive(Debug, Clone, Default)]
pub struct InputValues(HashMap<String, String>);

impl InputValues {
    pub fn new() -> Self {
        Self::default()
    }

    /// 取指定输入的值（未提供返回 None）。
    pub fn get(&self, name: &str) -> Option<&str> {
        self.0.get(name).map(|s| s.as_str())
    }

    /// 是否不含任何值。
    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }

    /// 以本值为基准，`over` 中的键仅补缺（本值优先）。
    /// 用于 config 覆盖（调用方传值覆盖/补全配置默认值）。
    pub fn merged_with(&self, over: &InputValues) -> Self {
        let mut merged = self.0.clone();
        for (k, v) in &over.0 {
            merged.entry(k.clone()).or_insert_with(|| v.clone());
        }
        Self(merged)
    }
}

impl From<HashMap<String, String>> for InputValues {
    fn from(map: HashMap<String, String>) -> Self {
        Self(map)
    }
}

/// 渲染结果：调用方决定注入位置（见各消费路径文档）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RenderedTask {
    /// 占位符替换后的任务文本。
    pub task: String,
    /// `## RULES` 块；无规则时为空串。
    pub rules: String,
}

/// 任务书渲染错误。
#[derive(Debug, thiserror::Error)]
pub enum TaskSpecError {
    #[error("missing required input '{0}' (no value and no default)")]
    MissingRequired(String),
    #[error("task references undeclared input '{0}'")]
    UnknownInput(String),
    #[error("input '{0}' is not a valid {1}")]
    InvalidType(String, InputTypeName),
}

/// 把 [`TaskSpecError`] 转换为 [`deepseeknova_core::DeepseeknovaError`]。
///
/// orphan rule：impl 放在拥有 `TaskSpecError` 的本 crate。`?` 可直接把
/// `Result<_, TaskSpecError>` 用于返回 `Result<_, DeepseeknovaError>` 的函数。
impl From<TaskSpecError> for deepseeknova_core::DeepseeknovaError {
    fn from(err: TaskSpecError) -> Self {
        deepseeknova_core::DeepseeknovaError::Agent(err.to_string())
    }
}

/// [`TaskSpecError::InvalidType`] 中可显示的输入类型名。
#[derive(Debug, Clone, Copy)]
pub enum InputTypeName {
    String,
    Number,
    Boolean,
}

impl fmt::Display for InputTypeName {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let s = match self {
            InputTypeName::String => "string",
            InputTypeName::Number => "number",
            InputTypeName::Boolean => "boolean",
        };
        f.write_str(s)
    }
}

/// 提取 task 中 `${{ inputs.<name> }}` 占位符引用的输入名（保持出现顺序，去重）。
fn referenced_inputs(task: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut rest = task;
    while let Some(start) = rest.find("${{ inputs.") {
        let after = &rest[start + "${{ inputs.".len()..];
        let Some(end) = after.find(" }}") else {
            break;
        };
        let name = after[..end].trim();
        if !name.is_empty() && !out.iter().any(|n| n == name) {
            out.push(name.to_string());
        }
        rest = &after[end..];
    }
    out
}

/// 值类型校验（只校验不转换）。
fn check_type(ty: &InputType, value: &str) -> Result<(), InputTypeName> {
    match ty {
        InputType::String => Ok(()),
        InputType::Number => value
            .parse::<f64>()
            .map(|_| ())
            .map_err(|_| InputTypeName::Number),
        InputType::Boolean => match value {
            "true" | "false" => Ok(()),
            _ => Err(InputTypeName::Boolean),
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn spec_with_inputs() -> TaskSpec {
        TaskSpec {
            name: "t".into(),
            task: "Analyze ${{ inputs.path }} with depth ${{ inputs.depth }}".into(),
            rules: vec!["Do not modify files".into()],
            inputs: vec![
                InputSpec {
                    name: "path".into(),
                    ty: InputType::String,
                    required: true,
                    default: None,
                },
                InputSpec {
                    name: "depth".into(),
                    ty: InputType::Number,
                    required: false,
                    default: Some("3".into()),
                },
            ],
            tools: vec!["read_file".into()],
            max_steps: 10,
        }
    }

    fn values(pairs: &[(&str, &str)]) -> InputValues {
        InputValues(
            pairs
                .iter()
                .map(|(k, v)| (k.to_string(), v.to_string()))
                .collect(),
        )
    }

    #[test]
    fn render_replaces_placeholders_and_builds_rules() {
        let out = spec_with_inputs()
            .render(&values(&[("path", "src/lib.rs")]))
            .unwrap();
        assert_eq!(out.task, "Analyze src/lib.rs with depth 3");
        assert_eq!(out.rules, "## RULES\n- Do not modify files\n");
    }

    #[test]
    fn render_caller_value_overrides_default() {
        let out = spec_with_inputs()
            .render(&values(&[("path", "a"), ("depth", "5")]))
            .unwrap();
        assert_eq!(out.task, "Analyze a with depth 5");
    }

    #[test]
    fn render_missing_required_errors() {
        let err = spec_with_inputs().render(&InputValues::new()).unwrap_err();
        assert!(matches!(err, TaskSpecError::MissingRequired(ref n) if n == "path"));
    }

    #[test]
    fn render_undeclared_placeholder_errors() {
        let spec = TaskSpec {
            task: "use ${{ inputs.typo }}".into(),
            ..spec_with_inputs()
        };
        let err = spec.render(&InputValues::new()).unwrap_err();
        assert!(matches!(err, TaskSpecError::UnknownInput(ref n) if n == "typo"));
    }

    #[test]
    fn render_unknown_values_keys_ignored() {
        let out = spec_with_inputs()
            .render(&values(&[("path", "a"), ("extra", "x")]))
            .unwrap();
        assert_eq!(out.task, "Analyze a with depth 3");
    }

    #[test]
    fn render_invalid_number_errors() {
        let err = spec_with_inputs()
            .render(&values(&[("path", "a"), ("depth", "abc")]))
            .unwrap_err();
        assert!(matches!(
            err,
            TaskSpecError::InvalidType(ref n, InputTypeName::Number) if n == "depth"
        ));
    }

    #[test]
    fn render_boolean_validation() {
        let spec = TaskSpec {
            task: "flag ${{ inputs.flag }}".into(),
            inputs: vec![InputSpec {
                name: "flag".into(),
                ty: InputType::Boolean,
                required: true,
                default: None,
            }],
            ..spec_with_inputs()
        };
        assert!(spec.render(&values(&[("flag", "true")])).is_ok());
        let err = spec.render(&values(&[("flag", "yes")])).unwrap_err();
        assert!(matches!(
            err,
            TaskSpecError::InvalidType(ref n, InputTypeName::Boolean) if n == "flag"
        ));
    }

    #[test]
    fn render_optional_without_default_uses_empty_string() {
        let spec = TaskSpec {
            task: "[${{ inputs.opt }}]".into(),
            inputs: vec![InputSpec {
                name: "opt".into(),
                ty: InputType::String,
                required: false,
                default: None,
            }],
            ..spec_with_inputs()
        };
        let out = spec.render(&InputValues::new()).unwrap();
        assert_eq!(out.task, "[]");
    }

    #[test]
    fn simple_spec_renders_empty_rules() {
        let spec = TaskSpec::simple("t", "do the thing", vec!["bash".into()], 5);
        let out = spec.render(&InputValues::new()).unwrap();
        assert_eq!(out.task, "do the thing");
        assert!(out.rules.is_empty());
        assert!(spec.inputs.is_empty());
    }

    #[test]
    fn merged_with_fills_missing_only() {
        let base = values(&[("a", "1")]);
        let over = values(&[("a", "9"), ("b", "2")]);
        let merged = base.merged_with(&over);
        let get = |k: &str| merged.0.get(k).cloned().unwrap_or_default();
        assert_eq!(get("a"), "1");
        assert_eq!(get("b"), "2");
    }

    /// 验证 `From<TaskSpecError> for DeepseeknovaError` 让 `?` 直接把
    /// `Result<_, TaskSpecError>` 用于返回 `Result<_, DeepseeknovaError>` 的函数。
    #[test]
    fn task_spec_error_converts_via_question_mark() {
        fn inner() -> Result<(), TaskSpecError> {
            Err(TaskSpecError::MissingRequired("input1".into()))
        }
        fn outer() -> Result<(), deepseeknova_core::DeepseeknovaError> {
            inner()?;
            Ok(())
        }
        let err = outer().unwrap_err();
        assert!(
            matches!(err, deepseeknova_core::DeepseeknovaError::Agent(_)),
            "应映射到 Agent 类别"
        );
    }
}
