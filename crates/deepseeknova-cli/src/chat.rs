use deepseeknova_core::chunk::Usage;
use deepseeknova_core::runner::{RunEvent, RunInput, Runner};
use deepseeknova_core::{Message, Role};
use deepseeknova_provider::factory::ReasoningEffort;
use deepseeknova_store::{SessionStore, StoredOutput};
use std::collections::HashMap;
use std::io::{BufRead, IsTerminal, Write};
use std::path::PathBuf;
use std::sync::Arc;
use tokio_stream::StreamExt;

/// 会话列表行：`(id, 首句预览, title, workspace)`。
type SessionRow = (String, String, Option<String>, Option<String>);

/// Session-persistence context for the chat REPL.
///
/// When present, every completed turn is appended to `store` under
/// `session_id`, and the `/sessions` / `/resume` commands operate on the same
/// store. `history` is the shared conversation buffer the agent reads from;
/// `/resume` replaces its contents in place so the next turn sees the restored
/// messages without rebuilding the agent.
pub struct ChatPersistence {
    /// Backing JSONL store.
    pub store: SessionStore,
    /// Id of the session currently being written.
    pub session_id: String,
    /// Number of turns already recorded in this session (monotonic counter).
    pub turn: u64,
    /// Shared conversation history the agent appends to and `/resume` rewrites.
    pub history: Arc<tokio::sync::Mutex<Vec<Message>>>,
    /// 会话标题存储（`/rename` 命名，落盘到 sessions 根目录 titles.json）。
    pub titles: SessionTitles,
    /// 工作区根路径（记录到每回合，会话按项目聚合用）。
    pub workspace: Option<String>,
}

/// 会话标题的磁盘存储：sessions 根目录下的 `titles.json`（id → title）。
///
/// 与 JSONL 会话文件分离，避免触碰 `StoredTurn` 格式；`list_sessions` 只
/// 匹配 `.jsonl` 后缀，`titles.json` 不会混入会话列表。
#[derive(Debug, Default)]
pub struct SessionTitles {
    path: PathBuf,
    titles: HashMap<String, String>,
}

impl SessionTitles {
    /// 从磁盘加载（文件不存在 → 空标题表）。
    pub fn load(path: PathBuf) -> Self {
        let titles = std::fs::read_to_string(&path)
            .ok()
            .and_then(|s| serde_json::from_str(&s).ok())
            .unwrap_or_default();
        Self { path, titles }
    }

    /// 查询会话标题（未命名返回 `None`）。
    pub fn get(&self, id: &str) -> Option<&str> {
        self.titles.get(id).map(String::as_str)
    }

    /// 设置会话标题并落盘（改名）。
    pub fn set(&mut self, id: &str, title: &str) -> anyhow::Result<()> {
        self.titles.insert(id.to_string(), title.to_string());
        self.save()
    }

    fn save(&self) -> anyhow::Result<()> {
        if let Some(parent) = self.path.parent() {
            std::fs::create_dir_all(parent).ok();
        }
        std::fs::write(&self.path, serde_json::to_string_pretty(&self.titles)?)?;
        Ok(())
    }
}

impl ChatPersistence {
    /// 会话标题（未命名返回 `None`）。
    pub fn session_title(&self, id: &str) -> Option<&str> {
        self.titles.get(id)
    }

    /// 重命名会话（`/rename <title>` 作用于当前会话）。
    pub fn rename(&mut self, id: &str, title: &str) -> anyhow::Result<()> {
        self.titles.set(id, title)
    }

    /// 会话列表（最新优先），每条为 `(id, 首句预览, title, workspace)`。
    pub fn list_sessions_with_titles(&self) -> anyhow::Result<Vec<SessionRow>> {
        Ok(self
            .store
            .list_sessions_with_preview()?
            .into_iter()
            .map(|(id, preview)| {
                let title = self.titles.get(&id).map(str::to_string);
                let workspace = self.store.session_workspace(&id);
                (id, preview, title, workspace)
            })
            .collect())
    }
}

/// Display mode for agent output.
#[derive(Clone, Copy, PartialEq, Eq)]
enum DisplayMode {
    /// Show text, reasoning, tool calls, results — everything.
    Normal,
    /// Show only text and tool results (hide reasoning).
    Lite,
    /// Show raw chunk types without formatting.
    Raw,
}

/// What the slash-command handler wants the main loop to do.
#[derive(Clone)]
enum SlashAction {
    Continue,
    Break,
    /// Rebuild the agent with a new reasoning effort and/or model.
    Rebuild {
        effort: Option<ReasoningEffort>,
        model: Option<String>,
    },
}

/// Run an interactive chat REPL session with rich slash commands.
///
/// `agent_factory` is called to (re-)create the agent when the session
/// starts or when the user changes model/reasoning-effort via `/model`
/// commands.  The factory receives the resolved effort level and an optional
/// model override and must return a boxed [`Runner`] + [`Send`].
///
/// When `persist` is `Some`, every completed turn is appended to the session
/// store and the `/sessions` / `/resume` commands become available.
///
/// When `router` is `Some`, the `/model use` (role-pointer hot switch) and
/// `/cost` (token/price report) commands become available.
pub async fn run_chat_repl<F>(
    agent_factory: F,
    baseline_effort: ReasoningEffort,
    initial_model: Option<String>,
    mut persist: Option<ChatPersistence>,
    router: Option<std::sync::Arc<deepseeknova_provider::router::ModelRouter>>,
) -> anyhow::Result<bool>
where
    F: Fn(Option<ReasoningEffort>, Option<String>) -> anyhow::Result<Box<dyn Runner + Send>>,
{
    let mut current_effort = baseline_effort;
    let mut current_model = initial_model;
    println!();
    // 启动横幅：行宽统一由 title/commands 行动态计算填充，避免硬编码
    // 空格数导致右边界错位（此前标题行比边框宽 5 列）。
    let banner_width = 50usize;
    let title = "     deepseeknova — interactive chat";
    let commands = "     /exit  /new  /model  /cost  /skills  /help";
    println!("╭{}╮", "─".repeat(banner_width));
    println!(
        "│{}{}│",
        title,
        " ".repeat(banner_width - title.chars().count())
    );
    println!(
        "│{}{}│",
        commands,
        " ".repeat(banner_width - commands.chars().count())
    );
    println!("╰{}╯", "─".repeat(banner_width));
    println!();

    let stdin = std::io::stdin();
    let mut reader = std::io::BufReader::new(stdin.lock());
    let mut mode = DisplayMode::Normal;

    let mut restart_requested = false;

    // Build initial agent via the factory.
    let mut runner = agent_factory(Some(current_effort), current_model.clone())?;

    loop {
        // Prompt
        let mode_indicator = match mode {
            DisplayMode::Normal => "",
            DisplayMode::Lite => " [lite]",
            DisplayMode::Raw => " [raw]",
        };
        let prompt = format!(">{mode_indicator} ");

        // TTY 走 raw 模式字符级编辑：退格按整字删除，中文不产生残渣；
        // 管道/重定向回退字节级 read_until + 残渣清理（原逻辑）。
        let line = if std::io::stdin().is_terminal() {
            match read_line_tty(&prompt)? {
                Some(l) => l,
                // EOF / Esc / Ctrl+C → 退出
                None => break,
            }
        } else {
            print!("{prompt}");
            std::io::stdout().flush().ok();

            let mut line_bytes: Vec<u8> = Vec::new();
            match reader.read_until(b'\n', &mut line_bytes) {
                Ok(0) => {
                    // EOF — exit
                    println!();
                    break;
                }
                Ok(_) => {
                    // read_until 保留行尾 \n
                    if line_bytes.last() == Some(&b'\n') {
                        line_bytes.pop();
                    }
                    // 管道输入不会由 TTY 字节级擦除产生残渣，但保持防御性清理。
                    let valid = utf8_prefix_len(&line_bytes);
                    line_bytes.truncate(valid);
                }
                Err(e) => {
                    eprintln!("read error: {e}");
                    break;
                }
            }
            String::from_utf8(line_bytes).unwrap_or_default()
        };

        let trimmed = line.trim();

        // Empty input — skip
        if trimmed.is_empty() {
            continue;
        }

        // Slash commands
        if let Some(cmd) = trimmed.strip_prefix('/') {
            let action = handle_slash_command(
                cmd,
                &mut mode,
                &mut restart_requested,
                &mut current_effort,
                baseline_effort,
                &mut current_model,
                persist.as_mut(),
                router.as_ref(),
            )
            .await?;
            match action {
                SlashAction::Break => break,
                SlashAction::Rebuild { effort, model } => {
                    // Merge: keep existing values unless command specifies new ones.
                    if let Some(e) = effort {
                        current_effort = e;
                    }
                    if let Some(m) = model {
                        current_model = Some(m);
                    }
                    match agent_factory(Some(current_effort), current_model.clone()) {
                        Ok(new_runner) => {
                            println!(
                                "switched: effort={effort_display}, model={model_display}",
                                effort_display = effort_label(current_effort),
                                model_display = current_model.as_deref().unwrap_or("(default)")
                            );
                            runner = new_runner;
                        }
                        Err(e) => {
                            eprintln!("failed to rebuild agent: {e}");
                        }
                    }
                }
                SlashAction::Continue => {}
            }
            continue;
        }

        // Send to agent
        let prompt_text = trimmed.to_string();
        let input = RunInput {
            prompt: prompt_text.clone(),
            images: Vec::new(),
            model_override: current_model.clone(),
        };

        match runner.run_stream(input).await {
            Ok(mut stream) => {
                println!();
                // 块级打印状态机：block_open=当前行有未收尾输出块（推理/正文/
                // 工具行）；reasoning_open=推理 dim 区间开启，收尾时闭合一次；
                // pending_usage=延迟到块边界显示的 token 统计（避免插在工具
                // 调用的 args 与结果之间）。
                let mut block_open = false;
                let mut reasoning_open = false;
                let mut text_started = false;
                let mut pending_usage: Option<Usage> = None;
                let mut final_output: Option<StoredOutput> = None;

                while let Some(event) = stream.next().await {
                    match event {
                        Ok(RunEvent::TextDelta(text)) => {
                            if mode == DisplayMode::Raw {
                                println!("[text] {text}");
                            } else {
                                if pending_usage.is_some() {
                                    flush_usage(&mut pending_usage, &mut block_open);
                                }
                                if !text_started {
                                    text_started = true;
                                    // 推理/工具行未收尾 → 先闭合样式再换行，
                                    // [回答] 标签独立成行，不与上游内容粘连。
                                    close_reasoning(&mut reasoning_open);
                                    close_block(&mut block_open);
                                    print!("\x1b[36m[回答]\x1b[0m ");
                                }
                                print!("{text}");
                                block_open = true;
                            }
                            std::io::stdout().flush().ok();
                        }
                        Ok(RunEvent::ReasoningDelta { text, .. }) => {
                            if mode == DisplayMode::Raw {
                                println!("[reasoning] {text}");
                            } else if mode == DisplayMode::Normal {
                                if pending_usage.is_some() {
                                    flush_usage(&mut pending_usage, &mut block_open);
                                }
                                if !reasoning_open {
                                    // 上一块未收尾 → 先换行，推理独立成段
                                    close_block(&mut block_open);
                                    // dim 区间只开一次，流式增量不重复包裹，
                                    // 收尾（正文/工具开始）时统一闭合。
                                    print!("\x1b[2m[思考]\x1b[0m \x1b[2m");
                                    reasoning_open = true;
                                }
                                print!("{text}");
                                block_open = true;
                                std::io::stdout().flush().ok();
                            }
                            // Lite mode: hide reasoning
                        }
                        Ok(RunEvent::ToolCallStart { name, .. }) => {
                            if mode == DisplayMode::Raw {
                                println!("[tool_start] {name}");
                            } else {
                                if pending_usage.is_some() {
                                    flush_usage(&mut pending_usage, &mut block_open);
                                }
                                // 推理/正文行未收尾 → 闭合样式并换行，
                                // 工具调用独立成行（此前会粘在推理文本尾部）。
                                close_reasoning(&mut reasoning_open);
                                close_block(&mut block_open);
                                print!("  \x1b[36m⚙ {name}\x1b[0m ...");
                                block_open = true;
                            }
                            std::io::stdout().flush().ok();
                        }
                        Ok(RunEvent::ToolCallEnd {
                            name: _, arguments, ..
                        }) => {
                            if mode == DisplayMode::Raw {
                                println!("[tool_end] args={}", truncate(&arguments, 200));
                            } else {
                                // 工具行收尾：换行后打印参数行
                                close_block(&mut block_open);
                                println!("     \x1b[2margs:\x1b[0m {}", truncate(&arguments, 200));
                            }
                        }
                        Ok(RunEvent::ToolResult { call_id: _, result }) => {
                            if mode == DisplayMode::Raw {
                                println!("[tool_result] {}", truncate(&result, 300));
                            } else {
                                close_block(&mut block_open);
                                println!("     \x1b[32m→\x1b[0m {}", truncate(&result, 300));
                            }
                        }
                        Ok(RunEvent::Usage(u)) => {
                            if mode == DisplayMode::Raw {
                                println!(
                                    "[usage] {}↑ {}↓ (cache hit:{} miss:{})",
                                    u.prompt_tokens,
                                    u.completion_tokens,
                                    u.cache_hit_tokens,
                                    u.cache_miss_tokens
                                );
                            } else {
                                // 延迟到下一块边界显示：usage 常夹在工具调用
                                // 链中（args 之后、结果之前），立即打印会撕裂
                                // 工具调用的视觉顺序。
                                pending_usage = Some(u);
                            }
                        }
                        Ok(RunEvent::Done(output)) => {
                            if mode != DisplayMode::Raw {
                                if pending_usage.is_some() {
                                    flush_usage(&mut pending_usage, &mut block_open);
                                }
                                close_reasoning(&mut reasoning_open);
                            }
                            close_block(&mut block_open);
                            // 轮次分隔线：多轮对话之间留出明确边界。
                            // 宽度取终端列数（上限 120 避免极端终端），失败回退 60。
                            let sep_width = crossterm::terminal::size()
                                .map(|(w, _)| (w as usize).clamp(20, 120))
                                .unwrap_or(60);
                            println!("\x1b[2m{}\x1b[0m", "─".repeat(sep_width));
                            // 最终文本已通过 TextDelta 流式打印完毕，此处
                            // 不再重复打印，只捕获最终输出供会话持久化使用。
                            final_output = Some(StoredOutput {
                                text: output.text.clone(),
                                tool_calls: Vec::new(),
                            });
                        }
                        Ok(RunEvent::TurnComplete) if block_open => {
                            close_block(&mut block_open);
                        }
                        Ok(RunEvent::Paused { reason, .. }) => {
                            if mode != DisplayMode::Raw {
                                if pending_usage.is_some() {
                                    flush_usage(&mut pending_usage, &mut block_open);
                                }
                                close_reasoning(&mut reasoning_open);
                            }
                            close_block(&mut block_open);
                            println!(
                                "\n⏸ paused: {reason} — 上下文已保留在本会话内存中，继续输入即可接着跑\
                                 （本轮进度尚未写入磁盘，退出进程后不会出现在 --resume 中）"
                            );
                        }
                        Err(e) => {
                            if mode != DisplayMode::Raw {
                                close_reasoning(&mut reasoning_open);
                                if pending_usage.is_some() {
                                    flush_usage(&mut pending_usage, &mut block_open);
                                }
                            }
                            eprintln!("\nerror: {e}");
                            break;
                        }
                        _ => {}
                    }
                }
                println!();

                // Persist the completed turn. The stored messages carry both
                // the user prompt and the assistant reply so `/resume` can
                // rebuild the conversation. A write failure only warns — it
                // never interrupts the session.
                if let Some(p) = persist.as_mut() {
                    if let Some(out) = final_output {
                        p.turn += 1;
                        let messages = vec![
                            Message {
                                role: Role::User,
                                content: prompt_text.clone(),
                                name: None,
                                tool_calls: None,
                                tool_call_id: None,
                                reasoning_content: None,
                            },
                            Message {
                                role: Role::Assistant,
                                content: out.text.clone(),
                                name: None,
                                tool_calls: None,
                                tool_call_id: None,
                                reasoning_content: None,
                            },
                        ];
                        let stored_input = RunInput {
                            prompt: prompt_text.clone(),
                            images: Vec::new(),
                            model_override: current_model.clone(),
                        };
                        let stored_turn = SessionStore::build_turn_with_workspace(
                            &stored_input,
                            p.turn,
                            messages,
                            Some(out),
                            p.workspace.as_deref(),
                        );
                        if let Err(e) = p.store.append(&p.session_id, &stored_turn) {
                            tracing::warn!("failed to persist chat turn: {e}");
                        }
                    }
                }
            }
            Err(e) => {
                eprintln!("error: {e}");
            }
        }
    }

    Ok(restart_requested)
}

/// Handle a slash command. Returns a [`SlashAction`] telling the main loop what to do.
// 私有 REPL 分发器：参数即命令所需的全部会话状态，拆结构体反而降低可读性。
#[allow(clippy::too_many_arguments)]
async fn handle_slash_command(
    cmd: &str,
    mode: &mut DisplayMode,
    restart_requested: &mut bool,
    current_effort: &mut ReasoningEffort,
    baseline_effort: ReasoningEffort,
    current_model: &mut Option<String>,
    persist: Option<&mut ChatPersistence>,
    router: Option<&std::sync::Arc<deepseeknova_provider::router::ModelRouter>>,
) -> anyhow::Result<SlashAction> {
    // Split command and optional arguments
    let (name, args) = cmd.split_once(' ').unwrap_or((cmd, ""));

    match name {
        // ── Exit ──────────────────────────────────────────────
        "exit" | "quit" | "q" => {
            println!("goodbye.");
            return Ok(SlashAction::Break);
        }

        // ── New session ───────────────────────────────────────
        "new" => {
            println!("starting a new session...");
            *restart_requested = true;
            return Ok(SlashAction::Break); // caller recreates the runner
        }

        // ── Clear screen ──────────────────────────────────────
        "clear" => {
            print!("\x1b[2J\x1b[H");
        }

        // ── Display mode ──────────────────────────────────────
        "raw" => match mode {
            DisplayMode::Normal => {
                *mode = DisplayMode::Lite;
                println!("display mode: lite (hiding reasoning content)");
            }
            DisplayMode::Lite => {
                *mode = DisplayMode::Raw;
                println!("display mode: raw (showing chunk types)");
            }
            DisplayMode::Raw => {
                *mode = DisplayMode::Normal;
                println!("display mode: normal");
            }
        },

        // ── Model info & control ──────────────────────────────
        "model" => {
            let (sub, sub_args) = args.split_once(' ').unwrap_or((args, ""));
            match sub {
                "" | "help" => {
                    println!("Model commands:");
                    println!("  /model                — show this help");
                    println!(
                        "  /model effort <level>  — set reasoning effort: \
                         disabled|low|medium|high|max"
                    );
                    println!("  /model thinking        — toggle thinking on/off");
                    println!("  /model switch <name>   — switch to a named provider model");
                    println!(
                        "  /model use <role> <name>  — set a role pointer: main|task|compact|quick"
                    );
                    println!();
                    println!(
                        "Current: effort={}, model={}",
                        effort_label(*current_effort),
                        current_model.as_deref().unwrap_or("(default)")
                    );
                    if let Some(r) = router {
                        use deepseeknova_provider::cost::ModelRole;
                        println!();
                        println!("Model pointers:");
                        for role in [
                            ModelRole::Main,
                            ModelRole::Task,
                            ModelRole::Compact,
                            ModelRole::Quick,
                        ] {
                            println!(
                                "  {:<8} → {}",
                                role.label(),
                                r.pointer(role).unwrap_or_else(|| "(default)".to_string())
                            );
                        }
                        println!(
                            "  (note: an explicit /model switch overrides the main pointer \
                             for this session)"
                        );
                    }
                }
                "effort" => {
                    if sub_args.is_empty() {
                        println!(
                            "Current reasoning effort: {} (config baseline: {})",
                            effort_label(*current_effort),
                            effort_label(baseline_effort)
                        );
                        println!("Usage: /model effort disabled|low|medium|high|max");
                    } else {
                        match parse_effort_command(sub_args) {
                            Ok(effort) => {
                                return Ok(SlashAction::Rebuild {
                                    effort: Some(effort),
                                    model: None,
                                });
                            }
                            Err(msg) => {
                                eprintln!("invalid effort level: {msg}");
                            }
                        }
                    }
                }
                "thinking" => {
                    let new_effort = toggle_thinking(*current_effort, baseline_effort);
                    println!(
                        "thinking {} → {}",
                        if current_effort.thinking() {
                            "on"
                        } else {
                            "off"
                        },
                        if new_effort.thinking() { "on" } else { "off" }
                    );
                    if new_effort != *current_effort {
                        return Ok(SlashAction::Rebuild {
                            effort: Some(new_effort),
                            model: None,
                        });
                    }
                }
                "switch" => {
                    if sub_args.is_empty() {
                        eprintln!("Usage: /model switch <provider-model-name>");
                    } else {
                        return Ok(SlashAction::Rebuild {
                            effort: None,
                            model: Some(sub_args.to_string()),
                        });
                    }
                }
                "use" => {
                    let mut parts = sub_args.split_whitespace();
                    match (parts.next(), parts.next(), router) {
                        (Some(role_s), Some(model), Some(r)) => {
                            match deepseeknova_provider::cost::ModelRole::parse(role_s) {
                                Some(role) => match r.set_pointer(role, model) {
                                    Ok(()) => {
                                        println!("pointer {} → {model}", role.label());
                                        // 重建 agent 使新指针生效（含委派引擎）
                                        return Ok(SlashAction::Rebuild {
                                            effort: None,
                                            model: None,
                                        });
                                    }
                                    Err(e) => eprintln!("{e}"),
                                },
                                None => {
                                    eprintln!("unknown role '{role_s}': main|task|compact|quick")
                                }
                            }
                        }
                        (_, _, None) => eprintln!("model pointers unavailable (no router)"),
                        _ => eprintln!("Usage: /model use <main|task|compact|quick> <model-name>"),
                    }
                }
                other => {
                    eprintln!("unknown /model sub-command: {other}");
                    eprintln!("try /model help");
                }
            }
        }

        // ── Cost accounting ─────────────────────────────────
        "cost" => match router {
            Some(r) => {
                let report = r.ledger().report(&r.price_table());
                if report.rows.is_empty() {
                    println!("no usage recorded yet");
                } else {
                    println!(
                        "{:<24} {:<8} {:>10} {:>12} {:>10} {:>10}",
                        "model", "role", "prompt", "completion", "cache-hit", "cost($)"
                    );
                    for row in &report.rows {
                        println!(
                            "{:<24} {:<8} {:>10} {:>12} {:>10} {:>10}",
                            row.model,
                            row.role.label(),
                            row.bucket.prompt_tokens,
                            row.bucket.completion_tokens,
                            row.bucket.cache_hit_tokens,
                            row.cost_usd
                                .map(|c| format!("{c:.4}"))
                                .unwrap_or_else(|| "-".to_string()),
                        );
                    }
                    if let Some(total) = report.total_usd {
                        println!("total estimated: ${total:.4}");
                    }
                    if report.unmetered_calls > 0 {
                        println!(
                            "note: {} call(s) had no usage info (not estimated)",
                            report.unmetered_calls
                        );
                    }
                }
            }
            None => println!("cost accounting unavailable (no router)"),
        },

        // ── Skills ────────────────────────────────────────────
        "skills" => {
            // Try to load skills from standard paths
            let paths = [".deepseeknova/skills", ".agents/skills"];
            let mut found = false;
            for path_str in &paths {
                let loader = deepseeknova_skills::SkillLoader::new(path_str);
                match loader.load_all() {
                    Ok(skills) if !skills.is_empty() => {
                        if !found {
                            println!("Available skills:");
                            found = true;
                        }
                        for skill in &skills {
                            println!("  • {} — {}", skill.name, skill.description);
                            if !skill.tools_allowed.is_empty() {
                                println!("    tools: {}", skill.tools_allowed.join(", "));
                            }
                        }
                    }
                    Ok(_) => {}
                    Err(e) => {
                        eprintln!("error loading skills from {path_str}: {e}");
                    }
                }
            }
            if !found {
                println!("No skills found. Create .md files in .deepseeknova/skills/");
            }
        }

        // ── MCP status ────────────────────────────────────────
        "mcp" => {
            println!("MCP servers are configured in deepseeknova.toml:");
            println!("  [[mcp_servers]]");
            println!("  name = \"my-server\"");
            println!("  command = \"npx\"");
            println!("  args = [\"-y\", \"@modelcontextprotocol/server-filesystem\"]");
            println!();
            println!("Use /mcp status to check connected servers (coming soon).");
        }

        // ── Sessions (list / resume / rename) ─────────────────────────
        "sessions" => match persist.as_ref() {
            Some(p) => match p.list_sessions_with_titles() {
                Ok(ids) if !ids.is_empty() => {
                    println!("Saved sessions (newest first):");
                    for (id, preview, title, workspace) in &ids {
                        let marker = if *id == p.session_id {
                            "  (current)"
                        } else {
                            ""
                        };
                        // title 优先；无命名回退 id（预览空时）。
                        let label = match (title, preview.is_empty()) {
                            (Some(t), _) => format!("{t}  ({id})"),
                            (None, false) => format!("{id} — {preview}"),
                            (None, true) => id.clone(),
                        };
                        // 工作区标注（非当前工作区才显示，避免噪声）。
                        let ws = workspace
                            .as_ref()
                            .filter(|w| **w != p.workspace.as_deref().unwrap_or(""))
                            .map(|w| format!("  [{}]", short_ws(w)))
                            .unwrap_or_default();
                        println!("  {label}{ws}{marker}");
                    }
                }
                Ok(_) => println!("(no saved sessions yet)"),
                Err(e) => eprintln!("failed to list sessions: {e}"),
            },
            None => println!("session persistence is disabled"),
        },

        "rename" => match persist {
            Some(p) => {
                let title = args.trim();
                if title.is_empty() {
                    eprintln!("Usage: /rename <title>  (renames the current session)");
                } else {
                    let current_id = p.session_id.clone();
                    match p.rename(&current_id, title) {
                        Ok(()) => println!("session renamed to '{title}'"),
                        Err(e) => eprintln!("failed to rename session: {e}"),
                    }
                }
            }
            None => println!("session persistence is disabled"),
        },

        "resume" => match persist {
            Some(p) => {
                let target = args.trim();
                if target.is_empty() {
                    eprintln!("Usage: /resume <session-id>  (see /sessions)");
                } else {
                    match p.store.load(target) {
                        Ok(turns) if !turns.is_empty() => {
                            let mut hist = p.history.lock().await;
                            hist.clear();
                            for t in &turns {
                                for m in &t.messages {
                                    hist.push(m.into());
                                }
                            }
                            let restored = hist.len();
                            drop(hist);
                            p.session_id = target.to_string();
                            p.turn = turns.len() as u64;
                            // 恢复时显示命名（有则显，无则略）。
                            let title = p
                                .session_title(target)
                                .map(|t| format!(" — '{t}'"))
                                .unwrap_or_default();
                            println!(
                                "resumed '{target}'{title} — {restored} messages across {} turns",
                                turns.len()
                            );
                        }
                        Ok(_) => eprintln!("session '{target}' is empty or does not exist"),
                        Err(e) => eprintln!("failed to load session '{target}': {e}"),
                    }
                }
            }
            None => println!("session persistence is disabled"),
        },

        // ── Undo ──────────────────────────────────────────────
        "undo" => {
            println!("Undo is not yet implemented in the CLI.");
            println!("Use the checkpoint system: crates/deepseeknova-checkpoint");
        }

        // ── Help ──────────────────────────────────────────────
        "help" => {
            println!("Commands:");
            println!("  /exit, /quit, /q  — end the session");
            println!("  /new              — start a new conversation");
            println!("  /clear            — clear the screen");
            println!("  /raw              — cycle display mode (normal/lite/raw)");
            println!("  /model            — show / change model & reasoning settings");
            println!("  /cost             — show per-model token usage & estimated cost");
            println!("  /skills           — list available agent skills");
            println!("  /mcp              — MCP server status");
            println!("  /sessions         — list saved sessions");
            println!("  /resume <id>      — restore a saved session's history");
            println!("  /rename <title>   — name the current session");
            println!("  /undo             — revert changes (coming soon)");
            println!("  /help             — show this help");
            println!();
            println!("Display modes:");
            println!("  normal  — text, reasoning, tool calls, results");
            println!("  lite    — hide reasoning content");
            println!("  raw     — show chunk types");
            println!();
            println!("Anything else is sent to the agent as a prompt.");
        }

        // ── Unknown ───────────────────────────────────────────
        other => {
            eprintln!("unknown command: /{other}");
            eprintln!("type /help for available commands.");
        }
    }

    Ok(SlashAction::Continue)
}

// ---------------------------------------------------------------------------
// Pure helpers (testable)
// ---------------------------------------------------------------------------

/// Parse a user-supplied reasoning-effort argument string into a
/// [`ReasoningEffort`].  Returns `Err(msg)` when the input isn't recognised.
fn parse_effort_command(args: &str) -> Result<ReasoningEffort, String> {
    let trimmed = args.trim();
    if trimmed.is_empty() {
        return Err("no level provided".into());
    }
    ReasoningEffort::from_config_str(trimmed)
        .ok_or_else(|| format!("unknown effort level: '{trimmed}'"))
}

/// Toggle thinking on/off: if currently enabled → disable; if disabled →
/// restore the baseline.  Always returns a new [`ReasoningEffort`]; the
/// caller decides whether to rebuild.
fn toggle_thinking(current: ReasoningEffort, baseline: ReasoningEffort) -> ReasoningEffort {
    if current.thinking() {
        ReasoningEffort::Disabled
    } else {
        baseline
    }
}

/// Human-readable label for a reasoning-effort level.
fn effort_label(effort: ReasoningEffort) -> &'static str {
    match effort {
        ReasoningEffort::Disabled => "disabled",
        ReasoningEffort::High => "high",
        ReasoningEffort::Max => "max",
    }
}

/// 收尾当前输出块：行未闭合则换行。
fn close_block(block_open: &mut bool) {
    if *block_open {
        println!();
        *block_open = false;
    }
}

/// 闭合推理 dim 样式区间（若开启）。
fn close_reasoning(reasoning_open: &mut bool) {
    if *reasoning_open {
        print!("\x1b[0m");
        *reasoning_open = false;
    }
}

/// 在打开新输出块之前显示缓存的 token 统计：若当前行有未收尾输出，
/// 先换行再打印，保证 usage 不粘连、不打断工具调用链。
fn flush_usage(pending: &mut Option<Usage>, block_open: &mut bool) {
    if let Some(u) = pending.take() {
        if *block_open {
            println!();
            *block_open = false;
        }
        eprintln!(
            "\x1b[2m  [{}↑ {}↓ {} total]\x1b[0m",
            u.prompt_tokens, u.completion_tokens, u.total_tokens
        );
    }
}

fn truncate(s: &str, max: usize) -> String {
    if s.len() <= max {
        s.to_string()
    } else {
        let end = s.floor_char_boundary(max);
        format!("{}…", &s[..end])
    }
}

/// 工作区路径的短标签：取最后一段（basename），空/根路径回退原样。
fn short_ws(path: &str) -> String {
    let normalized = path.replace('\\', "/");
    normalized
        .rsplit('/')
        .find(|s| !s.is_empty())
        .unwrap_or(path)
        .to_string()
}

/// 返回 `bytes` 中最长合法 UTF-8 前缀的长度。
///
/// TTY 规范模式的行编辑按字节擦除，删除中文等多字节字符时行尾会留下
/// 不完整的字符字节（残渣）；本函数从末尾回退截短，只丢弃残渣、保留
/// 前面所有完整内容。正常输入时整行合法，直接返回原长。
fn utf8_prefix_len(bytes: &[u8]) -> usize {
    let mut len = bytes.len();
    while len > 0 {
        if std::str::from_utf8(&bytes[..len]).is_ok() {
            return len;
        }
        len -= 1;
    }
    0
}

// ---------------------------------------------------------------------------
// 交互审批：REPL 的 Ask 裁决（权限门控默认开启后，写工具决策 Ask 需人工批准）
// ---------------------------------------------------------------------------

/// REPL 交互审批应答器：`Ask` 决策在终端询问 y/n。
///
/// - `y`/`yes` → 放行；其余输入（含回车默认）→ 拒绝（fail-closed）。
/// - Esc / Ctrl+C / EOF / 读取失败 → `None` → 拒绝（fail-closed）。
/// - 复用 `read_line_tty`（字符级编辑 + raw 模式 RAII），与主提示符同款交互；
///   审批发生在 agent 流式输出期间（主循环正 await 事件流），终端空闲，无
///   输入竞争。
struct ReplApprovalResponder;

#[async_trait::async_trait]
impl deepseeknova_core::runner::ApprovalResponder for ReplApprovalResponder {
    async fn request(&self, _id: &str, title: &str, description: Option<&str>) -> bool {
        // 审批信息走 stderr：stdout 保留给对话流（chat 正文/工具行）。
        eprintln!("\n[审批] {title}");
        if let Some(d) = description {
            for line in d.lines() {
                eprintln!("  {line}");
            }
        }
        match read_line_tty("  允许执行? [y/N] > ") {
            Ok(Some(line)) => matches!(line.trim(), "y" | "Y" | "yes"),
            _ => false,
        }
    }
}

/// 构造 REPL 审批应答器，供 CLI 注入 agent（`with_approval_responder`）。
pub(crate) fn repl_approval_responder(
) -> std::sync::Arc<dyn deepseeknova_core::runner::ApprovalResponder> {
    std::sync::Arc::new(ReplApprovalResponder)
}

// ---------------------------------------------------------------------------
// TTY raw-mode 行编辑：字符级退格/删除，中文不产生残渣。
// 复用 TUI 的 InputState（纯逻辑、UTF-8 安全），本实现只负责按键分派与
// 整行重绘；非 TTY（管道/重定向）走上方 read_until 回退路径。
// ---------------------------------------------------------------------------

/// 从 stdin 读一行。返回 `Ok(Some(line))` 为输入；`Ok(None)` 表示退出
/// （EOF / Esc / Ctrl+C / 读取失败）。调用方负责恢复后的换行排版。
fn read_line_tty(prompt: &str) -> std::io::Result<Option<String>> {
    use crossterm::event::{self, Event, KeyCode, KeyEventKind, KeyModifiers};
    use crossterm::QueueableCommand;
    use deepseeknova_tui::input::editor::InputState;
    use std::io::Write;

    let _guard = RawModeGuard::enter()?;
    let mut stdout = std::io::stdout();
    let mut input = InputState::default();

    loop {
        redraw_line(&mut stdout, prompt, &input)?;
        match event::read() {
            Ok(Event::Key(key)) if key.kind == KeyEventKind::Press => match key.code {
                KeyCode::Enter => {
                    stdout.queue(crossterm::cursor::MoveToNextLine(1))?;
                    stdout.flush()?;
                    return Ok(Some(input.text.clone()));
                }
                KeyCode::Backspace => input.backspace(),
                KeyCode::Delete => input.delete(),
                KeyCode::Left => input.move_left(),
                KeyCode::Right => input.move_right(),
                KeyCode::Home => input.home_line(),
                KeyCode::End => input.end_line(),
                KeyCode::Up => input.move_line_up(),
                KeyCode::Down => input.move_line_down(),
                KeyCode::Esc => return Ok(None),
                KeyCode::Char('c') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                    return Ok(None);
                }
                KeyCode::Char('d') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                    // Ctrl+D：空行退出（EOF 语义），否则删除光标后字符
                    if input.text.is_empty() {
                        return Ok(None);
                    }
                    input.delete();
                }
                KeyCode::Char('u') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                    input.clear()
                }
                KeyCode::Char('w') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                    input.delete_word_before()
                }
                KeyCode::Char('a') if key.modifiers.contains(KeyModifiers::CONTROL) => input.home(),
                KeyCode::Char('e') if key.modifiers.contains(KeyModifiers::CONTROL) => input.end(),
                KeyCode::Char(c)
                    if !key.modifiers.contains(KeyModifiers::CONTROL)
                        && !key.modifiers.contains(KeyModifiers::ALT) =>
                {
                    input.insert_char(c)
                }
                _ => {}
            },
            Ok(Event::Paste(text)) => {
                for c in text.chars() {
                    input.insert_char(c);
                }
            }
            Ok(_) => {}
            // stdin 关闭/不可读 → 视为退出
            Err(_) => return Ok(None),
        }
    }
}

/// 整行重绘：清行 → prompt + 文本 → 光标定位到正确列（Unicode 宽度）。
fn redraw_line<W: std::io::Write>(
    w: &mut W,
    prompt: &str,
    input: &deepseeknova_tui::input::editor::InputState,
) -> std::io::Result<()> {
    use crossterm::cursor::MoveToColumn;
    use crossterm::terminal::{Clear, ClearType::UntilNewLine};
    use crossterm::QueueableCommand;
    use unicode_width::UnicodeWidthStr;

    w.queue(MoveToColumn(0))?;
    w.queue(Clear(UntilNewLine))?;
    w.queue(crossterm::style::Print(prompt))?;
    w.queue(crossterm::style::Print(&input.text))?;
    let col = UnicodeWidthStr::width(prompt) + UnicodeWidthStr::width(&input.text[..input.cursor]);
    // 绝对定位：打印后光标已在文本末尾，MoveRight 会再右移 col 列导致
    // 光标与文字间距拉大，必须用 MoveToColumn。
    w.queue(MoveToColumn(col as u16))?;
    w.flush()
}

/// raw 模式 RAII 守卫：离开作用域自动恢复终端。
struct RawModeGuard;

impl RawModeGuard {
    fn enter() -> std::io::Result<Self> {
        crossterm::terminal::enable_raw_mode()?;
        Ok(Self)
    }
}

impl Drop for RawModeGuard {
    fn drop(&mut self) {
        let _ = crossterm::terminal::disable_raw_mode();
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    // ── utf8_prefix_len（TTY 字节级删除残渣清理）───────────────

    #[test]
    fn utf8_prefix_keeps_fully_valid_input() {
        assert_eq!(utf8_prefix_len(b"abc"), 3);
        assert_eq!(utf8_prefix_len("你好".as_bytes()), "你好".len());
        assert_eq!(utf8_prefix_len(b""), 0);
    }

    #[test]
    fn utf8_prefix_drops_trailing_residue_bytes() {
        // 删除中文后残留 1 字节（"好" 的 E5）在行尾：只丢 1 字节，保留"你"
        let mut bytes = b"abc".to_vec();
        bytes.extend_from_slice("你好".as_bytes());
        bytes.truncate(bytes.len() - 1); // 模拟删 1 字节留下残渣
        let len = utf8_prefix_len(&bytes);
        assert_eq!(&bytes[..len], b"abc\xe4\xbd\xa0", "完整内容保留，残渣丢弃");

        // 整行都是残渣（两个孤立字节）：全部丢弃
        assert_eq!(utf8_prefix_len(&[0xe5, 0xbd]), 0);

        // ASCII 与残渣混合
        assert_eq!(utf8_prefix_len(b"hi\xe5"), 2);
    }

    // ── parse_effort_command ───────────────────────────────────────────

    #[test]
    fn parse_effort_known_levels() {
        assert_eq!(
            parse_effort_command("disabled").unwrap(),
            ReasoningEffort::Disabled
        );
        assert_eq!(
            parse_effort_command("off").unwrap(),
            ReasoningEffort::Disabled
        );
        assert_eq!(
            parse_effort_command("none").unwrap(),
            ReasoningEffort::Disabled
        );
        assert_eq!(
            parse_effort_command("false").unwrap(),
            ReasoningEffort::Disabled
        );

        assert_eq!(parse_effort_command("high").unwrap(), ReasoningEffort::High);
        assert_eq!(
            parse_effort_command("medium").unwrap(),
            ReasoningEffort::High
        );
        assert_eq!(parse_effort_command("low").unwrap(), ReasoningEffort::High);

        assert_eq!(parse_effort_command("max").unwrap(), ReasoningEffort::Max);
        assert_eq!(
            parse_effort_command("maximum").unwrap(),
            ReasoningEffort::Max
        );
    }

    #[test]
    fn parse_effort_with_whitespace() {
        assert_eq!(
            parse_effort_command("  high  ").unwrap(),
            ReasoningEffort::High
        );
    }

    #[test]
    fn parse_effort_empty() {
        assert!(parse_effort_command("").is_err());
        assert!(parse_effort_command("   ").is_err());
    }

    #[test]
    fn parse_effort_unknown() {
        assert!(parse_effort_command("ultra").is_err());
        assert!(parse_effort_command("x-high").is_err());
    }

    // ── toggle_thinking ─────────────────────────────────────────────────

    #[test]
    fn toggle_thinking_disables_when_enabled() {
        assert_eq!(
            toggle_thinking(ReasoningEffort::High, ReasoningEffort::High),
            ReasoningEffort::Disabled
        );
        assert_eq!(
            toggle_thinking(ReasoningEffort::Max, ReasoningEffort::High),
            ReasoningEffort::Disabled
        );
    }

    #[test]
    fn toggle_thinking_restores_baseline_when_disabled() {
        assert_eq!(
            toggle_thinking(ReasoningEffort::Disabled, ReasoningEffort::High),
            ReasoningEffort::High
        );
        assert_eq!(
            toggle_thinking(ReasoningEffort::Disabled, ReasoningEffort::Max),
            ReasoningEffort::Max
        );
    }

    #[test]
    fn toggle_thinking_noop_when_baseline_is_disabled() {
        // Toggle off: disabled → disabled
        assert_eq!(
            toggle_thinking(ReasoningEffort::Disabled, ReasoningEffort::Disabled),
            ReasoningEffort::Disabled
        );
    }

    // ── effort_label ────────────────────────────────────────────────────

    #[test]
    fn effort_label_values() {
        assert_eq!(effort_label(ReasoningEffort::Disabled), "disabled");
        assert_eq!(effort_label(ReasoningEffort::High), "high");
        assert_eq!(effort_label(ReasoningEffort::Max), "max");
    }

    // ── 会话命名（SessionTitles / rename）────────────────────────────

    #[test]
    fn session_titles_set_get_and_persist_across_instances() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("titles.json");
        let mut titles = SessionTitles::load(path.clone());
        assert_eq!(titles.get("chat-a"), None, "未命名返回 None");
        titles.set("chat-a", "项目重构").unwrap();
        assert_eq!(titles.get("chat-a"), Some("项目重构"));
        // 新实例（跨进程）从磁盘读取标题。
        let reloaded = SessionTitles::load(path.clone());
        assert_eq!(reloaded.get("chat-a"), Some("项目重构"));
    }

    #[test]
    fn chat_persistence_rename_and_list_titles() {
        let dir = tempfile::tempdir().unwrap();
        let store = SessionStore::new(dir.path().to_path_buf()).unwrap();
        let input = RunInput {
            prompt: "hello".into(),
            images: vec![],
            model_override: None,
        };
        let turn = SessionStore::build_turn(&input, 1, vec![], None);
        store.append("chat-a", &turn).unwrap();
        store.append("chat-b", &turn).unwrap();

        let mut p = ChatPersistence {
            store,
            session_id: "chat-a".into(),
            turn: 1,
            history: Arc::new(tokio::sync::Mutex::new(Vec::new())),
            titles: SessionTitles::load(dir.path().join("titles.json")),
            workspace: Some("/tmp/proj".into()),
        };
        assert_eq!(p.session_title("chat-a"), None);
        p.rename("chat-a", "核心重构").unwrap();
        assert_eq!(p.session_title("chat-a"), Some("核心重构"));

        let metas = p.list_sessions_with_titles().unwrap();
        let a = metas
            .iter()
            .find(|(id, _, _, _)| id == "chat-a")
            .expect("chat-a 在列表中");
        assert_eq!(a.2.as_deref(), Some("核心重构"), "改名生效并出现在列表");
        let b = metas
            .iter()
            .find(|(id, _, _, _)| id == "chat-b")
            .expect("chat-b 在列表中");
        assert_eq!(b.2, None, "未命名会话 title 为 None");
    }
}
