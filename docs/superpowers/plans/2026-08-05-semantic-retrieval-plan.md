# 任务书：记忆语义检索（embedder）最小闭环（2026-08-05）

## 1. 意图
把记忆召回从「关键词命中」升级为「语义相关」：启用后可配 OpenAI 兼容嵌入服务
（/v1/embeddings），写入即生成向量、旧记忆可显式回填，召回按 bm25 + 余弦 +
生命周期信号融合排序。干完：`[memory] embedder="remote"` 配好模型和密钥后，
recall 能找回换说法但同义的记忆；不配置时行为和现在完全一致。

## 2. 我替领导拍的板
- 只做 remote 后端：复用 provider 已有 reqwest/tokio，零新增外部依赖；local 本轮
  不实现，配置为 remote 前校验不足时给明确报错（fail-open 到纯 FTS）。
- API key 从环境变量读（DEEPSEEKNOVA_EMBED_API_KEY，回落 OPENAI_API_KEY），
  不落配置文件/日志。
- 失败一律 fail-open：缺密钥/网络错/解析错 → warn 日志 + 回落纯 FTS，绝不打断
  运行、绝不 panic。
- 默认 embedder="none" 不变；嵌入是写入后的尽力而为动作，失败不回滚写入。
- 旧库不自动全量回填（避免启动即打满网络），提供 `memory embed-backfill` 显式回填。
- 既有公开 API 签名不动：新增 open_with_embedder / search_hybrid_with_weight，
  旧入口委托新实现。
- 背景：上一轮 CLOSEOUT 无遗留阻塞；BLOCKED 待裁决「语义检索 embedder」本轮落地，
  其余四项（TUI 协议面板/计划载体/多模型反思/记忆清理 UI）留待下轮。

## 3. 白名单（只改这些，其余只读）
- crates/deepseeknova-core/src/memory/（embedding.rs / engine.rs / store.rs）
- crates/deepseeknova-provider/src/embeddings.rs（新）+ lib.rs（仅 mod 声明）
- crates/deepseeknova-config/src/lib.rs（仅 MemoryConfig 段 + merge + 测试）
- crates/deepseeknova-runtime/src/lib.rs（仅记忆装配段 + 测试）
- crates/deepseeknova-cli/src/main.rs + cli.rs（仅 memory 子命令）
- crates/deepseeknova-tools/src/memory.rs（仅测试）
- GUIDE.md、CHANGELOG.md、PROGRESS.md、BLOCKED.md、crates/REVIEW.md、crates/CLOSEOUT.md
- 禁改：core 既有公开 API 签名、既有测试断言、schema 预算测试、CI/Makefile、
  Cargo.toml 依赖（不新增）。

## 4. 任务
- 任务 0：前提核验（已实测）：feat/memory-lifecycle@e941f14 工作树干净；make check
  EXIT=0、0 failed、2 既有 ignored。基线测试数：core 132、agent 231、provider 35、
  runtime 52、cli 32、config 33+18、tools 66+12+7。
- 任务 1：store 混合检索融合。新增 search_hybrid_with_weight(query, limit, provider,
  model, rank_weight)：FTS 基数改纯 bm25（weight=0，修掉现有 search_hybrid 用带生命
  周期分数的 FTS 再归一的双重计权）；嵌入独有命中补尾；最终分 =
  0.5*bm25归一 + 0.5*cosine + rank_weight*lifecycle_factor（stage/importance/recency
  与 run_memory_search 同款语义）。search_hybrid 委托新方法（DEFAULT_RANK_WEIGHT）。
  测试 ≥3：语义独有命中（无 FTS 词也召回）、融合排序、rank_weight=0 与纯 0.5/0.5
  等价（组合）。
- 任务 2：provider RemoteEmbedder。embeddings.rs：from_parts(base_url, api_key,
  model, timeout) + from_memory_config（校验 embedder=remote、model 非空、读 env）；
  POST {base_url}/embeddings 带 Bearer；解析 data[0].embedding；内部持独立 tokio
  runtime block_on（不阻塞调用方 runtime）。测试 ≥4（本地 TcpListener 端到端）：
  请求路径/Bearer/body 正确、成功解析、HTTP 500 → Err、坏 JSON → Err。
- 任务 3：config。MemoryConfig 增 embed_base_url（默认 https://api.openai.com/v1）、
  embed_timeout_secs（默认 30）；merge 按非默认覆盖；embed_model 文档注明 remote
  必填。测试 ≥2：默认值、TOML 解析 + merge。
- 任务 4：engine 接线。open_with_embedder / open_in_memory_with_embedder（旧入口
  委托 None）；写入即嵌入（remember、record_task 三入口、record_knowledge，同模型
  已有向量则跳过）；recall/recall_with_weight 在有 provider 时走
  search_hybrid_with_weight；backfill_embeddings（跳过 archived，返回 (attempted,
  ok)）；stats 增 embedded 计数。测试 ≥5：写入后 get_embedding 有值、语义独有命中、
  provider 失败写入仍成功（fail-open）、backfill 计数、stats embedded。
- 任务 5：runtime + CLI 装配。provider 增 try_memory_embedder(&MemoryConfig) ->
  Option<Arc<dyn EmbeddingProvider>>（失败 warn + None）；runtime 记忆装配段使用；
  CLI memory stats 打印 embedded=N/total=M；CLI 增 memory embed-backfill。
  runtime 测试 ≥1（remote 无密钥 → 装配成功、recall 回落 FTS）；CLI 冒烟两条。
- 任务 6：文档 + 收尾。GUIDE 记忆节补 embedder 配置/回填/CLI；CHANGELOG Added；
  cargo fmt；make check 全绿；反向验证（改坏语义独有命中断言 → 真红 → 还原 → 真绿）；
  提交分支 feat/semantic-retrieval（不 push）。

## 5. 防作弊
- 测试数只增不减：core ≥ 138、provider ≥ 39、config ≥ 53、runtime ≥ 53、
  tools ≥ 67、workspace 0 failed（2 既有 ignored 除外）。
- 不许 mock 被测对象：provider 用真实本地 HTTP 服务端到端测；engine 测试用
  确定性向量 fake provider（接口替身，允许）；不许删测试/放宽断言/|| true/跳过 fmt。
- 新行为必有测试；组合测试：hybrid + rank_lifecycle_weight=0 等价纯 0.5/0.5；
  远程嵌入 HTTP 端到端。
- 反向验证必须贴真红→真绿输出。

## 6. 完成条件（两条硬指标 + 止损）
- 硬指标 A：make check EXIT=0；workspace 0 failed；core ≥ 138、provider ≥ 39、
  config ≥ 53、runtime ≥ 53、tools ≥ 67。
- 硬指标 B：`cargo test -p deepseeknova-core memory::` 与
  `cargo test -p deepseeknova-provider` 全绿；CLI 冒烟：temp 目录跑 memory stats
  输出含 embedded=、memory embed-backfill（无 provider）不 panic 且 attempted=0。
- 止损：同一验收连败 3 次换路径；结果比基线差回滚如实报告；超 3 小时停手汇报。
