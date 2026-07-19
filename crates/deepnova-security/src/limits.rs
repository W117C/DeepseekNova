use std::time::Duration;

#[derive(Debug, Clone)]
pub struct ResourceLimits {
    pub max_files: usize,
    pub max_file_size: u64,
    pub max_total_read_bytes: u64,
    pub max_execution_time: Duration,
    pub max_output_bytes: u64,
    pub max_tool_calls: usize,
}

impl Default for ResourceLimits {
    fn default() -> Self {
        Self {
            max_files: 500,
            max_file_size: 1024 * 1024,             // 1 MB
            max_total_read_bytes: 50 * 1024 * 1024, // 50 MB
            max_execution_time: Duration::from_secs(120),
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
            Duration::from_secs(120),
            "max_execution_time should be 120s"
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
