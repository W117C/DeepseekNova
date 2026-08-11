/// 能力枚举：需要被安全上下文显式授予方可执行的特权操作。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Capability {
    /// 读取文件。
    FileRead,
    /// 写入/修改文件。
    FileWrite,
    /// 执行 shell 命令。
    CommandExecute,
    /// 访问网络（HTTP/WebFetch 等）。
    NetworkAccess,
    /// 调用 MCP 工具。
    McpInvoke,
    /// 读取记忆存储。
    MemoryRead,
    /// 写入记忆存储。
    MemoryWrite,
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashSet;

    #[test]
    fn test_capability_all_unique() {
        let all = [
            Capability::FileRead,
            Capability::FileWrite,
            Capability::CommandExecute,
            Capability::NetworkAccess,
            Capability::McpInvoke,
            Capability::MemoryRead,
            Capability::MemoryWrite,
        ];
        let mut set = HashSet::new();
        for cap in all {
            assert!(set.insert(cap), "duplicate capability: {:?}", cap);
        }
        assert_eq!(set.len(), 7);
    }

    #[test]
    fn test_capability_eq() {
        assert_eq!(Capability::FileRead, Capability::FileRead);
        assert_ne!(Capability::FileRead, Capability::FileWrite);
    }

    #[test]
    fn test_capability_copy() {
        let a = Capability::NetworkAccess;
        let b = a; // Copy
        assert_eq!(a, b);
    }
}
