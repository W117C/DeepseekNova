#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Capability {
    FileRead,
    FileWrite,
    CommandExecute,
    NetworkAccess,
    McpInvoke,
    MemoryRead,
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
