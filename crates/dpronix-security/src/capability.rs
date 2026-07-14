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
