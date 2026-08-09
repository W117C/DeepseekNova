/// 嵌入检索支撑：嵌入提供器抽象与余弦相似度工具。
pub mod embedding;
/// 统一记忆门面：把持久 MemoryStore 包成运行路径与 CLI 共用的引擎。
pub mod engine;
/// 记忆证据：记录频次、置信度与多源观察时间窗。
pub mod evidence;
/// 记忆生命周期：基于召回频次、年龄与重要性的阶段晋级/降级。
pub mod lifecycle;
/// 记忆晋升策略：基于证据的阶段迁移评估。
pub mod policy;
/// 用户画像：持久化的偏好与行为模式。
pub mod profile;
/// 跨会话召回引擎：在每轮对话起点注入相关记忆。
pub mod recall;
/// 秘密脱敏：写入记忆库前抹除常见密钥/token。
pub mod redact;
/// 自动技能系统：从任务经验抽取可复用技能。
pub mod skill;
/// 持久化记忆库：SQLite + FTS5 全文检索后端。
pub mod store;
