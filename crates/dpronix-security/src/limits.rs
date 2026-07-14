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
