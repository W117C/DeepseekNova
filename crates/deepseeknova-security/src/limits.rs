use std::time::Duration;

/// 资源限制：约束工具执行消耗的各类配额。
#[derive(Debug, Clone)]
pub struct ResourceLimits {
    /// 单次任务最多处理/涉及的文件数。
    pub max_files: usize,
    /// 单个文件的最大字节数。
    pub max_file_size: u64,
    /// 单次任务累计读取字节数上限。
    pub max_total_read_bytes: u64,
    /// 单次任务最长执行时长。
    pub max_execution_time: Duration,
    /// 单次输出最大字节数。
    pub max_output_bytes: u64,
    /// 单次任务最多工具调用次数。
    pub max_tool_calls: usize,
}

impl Default for ResourceLimits {
    fn default() -> Self {
        Self {
            max_files: 500,
            max_file_size: 1024 * 1024,             // 1 MB
            max_total_read_bytes: 50 * 1024 * 1024, // 50 MB
            max_execution_time: Duration::from_secs(600),
            max_output_bytes: 10 * 1024 * 1024, // 10 MB
            max_tool_calls: 100,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_default_limits_are_reasonable() {
        let limits = ResourceLimits::default();
        assert_eq!(limits.max_files, 500, "max_files should be 500");
        assert_eq!(
            limits.max_file_size, 1_048_576,
            "max_file_size should be 1 MB"
        );
        assert_eq!(
            limits.max_total_read_bytes, 52_428_800,
            "max_total_read_bytes should be 50 MB"
        );
        assert_eq!(
            limits.max_execution_time,
            Duration::from_secs(600),
            "max_execution_time should be 600s"
        );
        assert_eq!(
            limits.max_output_bytes, 10_485_760,
            "max_output_bytes should be 10 MB"
        );
        assert_eq!(limits.max_tool_calls, 100, "max_tool_calls should be 100");
    }

    #[test]
    fn test_limits_can_be_partially_overridden() {
        let limits = ResourceLimits {
            max_files: 10,
            ..ResourceLimits::default()
        };
        assert_eq!(limits.max_files, 10);
        assert_eq!(limits.max_file_size, 1_048_576); // inherited from default
    }
}
