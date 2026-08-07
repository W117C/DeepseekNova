# 新星观测台（Nova Observatory）— TUI 与桌面端设计规范

> 状态：设计已定稿（2026-08-07），实现待启动。
> 决策链：PRODUCT.md（产品事实）→ 方向轮（掷签分配「新星观测台」，seed key `57f4f686`；用户第一轮选品类标准，第二轮改选观测台，用户决定为准）→ 构图轮（三稿，用户批准两稿：B「观测工作台」为桌面端主窗口北极星，见 `.impeccable/mocks/obs-comp-b-workbench.png`（sidecar `approved: true`）；A「观测日志」选定为观测日志视图（归档/日志页），见 `.impeccable/mocks/obs-comp-a-log.png`（sidecar `approved: true`）；C「红光联锁焦点版」落选）。
> Surface brief：`.impeccable/surfaces/crates-deepseeknova-desktop.md`、`.impeccable/surfaces/crates-deepseeknova-tui.md`（含保真度清单，build 时对照）。
> 本文件是视觉/交互设计规范；架构史在 DESIGN.md（勿混写）。

---

## 一、视觉世界

**一句话**：Agent 会话即一夜天文观测——流式事件是观测记录，安全审批是圆顶联锁，评分卡是测光曲线。产品机制（安全优先 + 质量闭环）经由观测台的仪器纪律成为可感知的界面气质。

**两端一个世界**：桌面端是完整版渲染（web 技术），TUI 是字符版渲染（ratatui）。语义 token 同一张表，改一处两端同义。

**风险自觉**（执行红线）：夜空底色若执行松散会滑回品类默认的「暗色+霓虹」AI 皮肤。约束手段：无辉光、无渐变、flat matte；一切装饰必须是**制图元素**（刻线、刻度、星点、表格），装饰密度受限（graticule 底纹透明度 ≤6%）；数据一律 mono + tabular-nums 的表格纪律。

### 1.1 设计令牌（语义色，两端共用一张表）

| 语义 | 夜空（暗色，旗舰默认） | 印刷星图（浅色） | TUI 现值（theme.rs） |
|---|---|---|---|
| bg / 底 | `#0B1020` | `#F7F8FC` 纸白 | 终端默认底 |
| bg-panel / 图框内 | `#0F1528` | `#FFFFFF` | — |
| hairline / 刻线 | `#232C4A` | `#D8DDEC` | DIM |
| graticule 底纹 | `#232C4A` @ ≤6% | `#D8DDEC` @ ≤8% | 无（终端不铺底纹） |
| ink / 正文 | `#EDF1FB` | `#1A2138` | 终端默认前景 |
| ink-dim / 次要 | `#8A93B0` | `#6A7390` | DIM |
| accent / 星色·用户·主动作 | `#4D6BFE`（品牌蓝） | `#3B55D9`（深化保对比） | RGB(77,107,254) bold |
| agent / 模型语声 | `#7A8CFF` | `#5A4FB8` | RGB(122,140,255) |
| reasoning / 推理 | dim + italic | dim + italic | DIM+ITALIC |
| success / 完成 | `#3FB96B` | `#0E7A42` | Green |
| warn / 中断·标注 | `#E8A33D`（琥珀） | `#B87514` | Yellow（预算条 >80%） |
| danger / 失败·危险·红光 | `#D5484F` | `#C0303A` | Red（错误加粗） |
| selection / 选中 | `#263264` | `#DDE4FB` | bg RGB(38,50,100)+REVERSED |

约束：accent 是唯一星色；琥珀仅作标注/中断；绿/红仅作状态语义（红＝观测台红光，保留给危险与失败）。TUI 侧 `theme.rs` 现值已与本表一致（accent/agent/selection 完全同值），浅色档需按「印刷星图」列补齐。

### 1.2 字体排印

- **桌面端**：UI 用系统 sans 栈（`-apple-system, "SF Pro", "Segoe UI", "Noto Sans SC", sans-serif`）；数据（命令、路径、时间戳、分值、diff）一律 mono 栈（`ui-monospace, "SF Mono", "JetBrains Mono", monospace`）+ `font-variant-numeric: tabular-nums`。坡度：对话正文 14px/1.65；会话标题 13px；标签与时间戳 11-12px；面板标题 12px 加字距；无超大展示字号——观测台的权威来自纪律而非音量。
- **TUI**：继承终端字体，靠 bold/dim/italic 三态与语义色表达坡度。
- CJK 注意：mono 栈须验证中文回退（PingFang SC 等宽混排），diff 与表格对齐以西文等宽列为准。

### 1.3 组件语法

- **图框（chart frame）**：1px 细边框 + 8px 圆角 + 左上角 12px 小标题——TUI 的 ╭─╮ 圆角单线框的 web 对应物。分层靠刻线，无阴影。
- **星点（magnitude dots）**：状态与活跃度用大小三档（2/3/4px）的圆点表达；运行中脉动（respect `prefers-reduced-motion`）。
- **刻度尺（scales）**：token 预算、成本等连续量一律画成带刻度的标尺，不用无刻度胖进度条。
- **字符语言在桌面端的延续**：`❯` 提示符、`▸` 折叠、`✓/✗`、`●/○` 直接复用，两端肌肉记忆同源。

### 1.4 动效语法

动效即观测仪器的运动，一次编排、全局复用：星座图连线随事件到达生长（SVG stroke-dashoffset）；运行中星点 2s 呼吸脉动；流式文本按到达渲染（无打字机音效式抖动）；面板切换 ≤150ms 淡入。`prefers-reduced-motion` 下全部退化为静态。

---

## 二、桌面端设计

### 2.1 技术架构

- **栈**（PRODUCT.md 已拍板）：Tauri 2 + SolidJS 1.9 + Tailwind CSS 4 + shiki（代码/diff 高亮）；git 历史 `3ab55d7^` 的第二代工程可考古（IPC 适配层、Vite 配置）。
- **后端通道**：桌面端进程内以 sidecar 方式托管 `deepseeknova-serve`（随机端口 + 每次启动生成的 bearer token，经 Tauri 注入 webview），前端统一走 HTTP/SSE——**单一 API 面**，不重建 61 个 tauri command 的旧路（第一代教训：命令面爆炸）。仅窗口/系统集成（托盘、通知、文件对话框）走 Tauri command。
- 也可连接外部 serve 实例（设置中配 URL + token），同一前端两种部署。

### 2.2 信息架构（批准构图 B）

```
┌──────────────────────────────────────────────────────────────┐
│ 左侧栏 1/5          │ 观测之夜 runs 条带        全部 runs → │
│ DeepseekNova [新会话]├──────────────────────────┬─────────────┤
│ 观测日志            │ 对话流（划线日志）        │ 本次观测    │
│ ▾ 08-07 夜  6       │  你 / deepseek-v4-pro    │ （星座图）  │
│  ● run-…-001 23:47  │  ▸ 推理过程（dim 折叠）  ├─────────────┤
│  · run-…-002 22:15  │  ⚙ 工具记录（表格行）    │ 测光·评分卡 │
│ ▸ 08-06 夜  5       │  diff / 代码块（shiki）  │ 治理 ──── 92│
│ …                   ├──────────────────────────┤ …          │
│ ⚙ 设置              │ ❯ composer + 模型chip    │ 综合 ━━━ 91 │
└──────────────────────────────────────────────────────────────┘
```

- **左侧栏 · 观测日志**：会话按夜次（日期）分组，条目 = 星点（星等三档=活跃度）+ 标题 + mono 时间；活跃项细圆角靛蓝框。底部：设置入口 + serve 连接状态点。
- **runs 条带 · 观测之夜**：当前与近期 run 的 chip（●运行中 / ⏸已中断+↻恢复 / ✓已完成），点击切换观测对象；「全部 runs →」进归档页。
- **对话流**：划线日志条目——角色标签（你 / 模型名）+ mono 时间戳右对齐；推理默认折叠为 dim 斜体行；工具调用是可展开的划线表格行（⚙ + mono 命令 + 状态 + 耗时），展开见输出；diff 红绿行、代码块 shiki 高亮；流式 pending 用星点脉动。
- **composer**：细圆角图框 + `❯`；`@` 文件补全、`/` 命令补全（与 TUI 同一命令词表）；模型 chip 与 token 刻度尺就地显示。
- **右栏上 · 本次观测（星座图）**：run 事件链的 SVG 星座——节点星点（大小=事件权重：工具调用>消息>心跳；颜色=状态），细线连接，节点 mono 微标签；运行中节点脉动；**点击星点滚动到对应日志条目**（签名交互）。密度承诺：典型 run 6-14 节点铺满图框 2/3，空 run 显示空 graticule +「等待首个事件」。
- **右栏下 · 测光·评分卡**：六维光度表（治理/验证/反思/审查/协议/综合），细靛蓝横条 + mono 分值，综合行强调；run 结束后从 `/v1/sessions/{id}/scorecard` 拉取，未产出时显示「测光待完成」。

### 2.3 审批（圆顶联锁）

构图 C 的整卡红光被否决；采用克制版：**composer 上方停靠卡**——细红边框 `#D5484F`、盾形字形、标题「权限请求」、mono 完整命令、风险标签（**只读/非只读/危险**，来自 security readonly 分类器）、按钮「允许 (Y)」（非只读/危险=红实心，只读=accent 实心）/「拒绝 (N)」轮廓 /「仅本次 ▾」（加入规则）。挂起时星座图当前节点变红脉动。fail-closed 语义显式呈现：流断开=自动拒绝，卡上注明。危险级（hard_deny）不出卡，直接以红色系统条目落入日志流并说明不可覆盖。

### 2.4 其余界面

- **全部 runs（观测归档）**：表格纪律的 run 列表（状态星点、标题、夜次、耗时、成本、六维综合分），过滤器（状态/日期），行动作：恢复（`POST /v1/runs/{id}/resume`）、看诊断、看评分卡。本页构图 = A「观测日志」（`.impeccable/mocks/obs-comp-a-log.png`）：顶部圆顶状态带 + 夜次分组会话栏 + 右缘竖向时间线标尺（事件星点 + 底部预算刻度）。
- **诊断报告视图**：失败会话的阶段时序画成横向时间轴（阶段=刻度段，失败点=红星），下方失败详情/子代理链/findings 折叠区；数据 `GET /v1/sessions/{id}/diagnose`。
- **评分卡聚合页**：`/v1/metrics/scorecards` 的均值/趋势/最差维度，趋势画成测光曲线（折线 + 星点采样）。
- **设置**：连接（内嵌/外部 serve、token）、Provider 与模型、权限规则编辑器（allow/ask/deny 列表 + 风险说明）、主题（夜空/印刷星图/跟随系统）、快捷键、语言。
- **首启 onboarding**：欢迎卡（圆角图框 + 圆顶字形 + 「开始第一夜观测」），检测/启动内嵌 runtime，一步进入新会话；空状态一律「空 graticule + 一句引导」，不留白板。

### 2.5 键盘与无障碍

- `Cmd+K` 命令面板（与 TUI 共用命令注册表词表）、`Cmd+N` 新会话、`Y/N` 审批、`J/K` 消息导航、`Cmd+\` 折叠右栏。
- 正文对比度 ≥4.5:1（`#8A93B0` on `#0B1020` ≈ 7.2:1 ✓；浅色档同验）；焦点环 accent 2px；星座图节点提供文本等价（时间轴列表即其可达替代）；全部动效respect reduced-motion。

### 2.6 配套后端缺口（已确认先行补齐）

1. **会话级 HTTP API**：列表/创建/恢复会话与历史消息（现状：TUI 走本地 `SessionController` 注入，serve 只有 run 粒度）。
2. **本地认证**：serve 仅限 127.0.0.1 且无 auth；桌面端 sidecar 模式需每启动随机 bearer token；外连模式需可配 token。

---

## 三、TUI 演进设计

原则：**精修不换世界**——保留全部键位语法、斜杠命令名、布局骨架与母题；观测台语义以字符语言克制表达。

1. **语义 token 对齐**：`theme.rs` 维持唯一来源；按 §1.1 表补齐浅色档（印刷星图），dark 档现值不动。
2. **会话面板夜次分组**：侧边栏会话列表加日期分组头（`08-07 夜`），条目星点沿用 `●`（活跃度可用 `·/●/◉` 三档）。
3. **审批浮层升级**：现「🔒 请求授权」红框基础上，加风险标签行（只读/非只读/危险）与完整 mono 命令展示，`y/n` 语义不变；危险级说明「策略硬拒，不可覆盖」。
4. **测光评分卡**：侧边栏成本面板扩为「测光」面板——六维 `█░` 横条 + 分值；新增 `/scorecard` 斜杠命令输出六维摘要（数据源同 serve 落盘 JSON）。
5. **欢迎卡缀圆顶字形**：50 列圆角欢迎卡标题行加 `⌒` 圆顶字符点缀（单字符级，克制）。
6. **信息层级演进延续**：当前未提交的 notice 通道方向（瞬态反馈 6s TTL、永久内容进流）与本设计一致，继续推进。
7. **文档同步**：GUIDE.md「配色与主题」章节与 `theme.rs` 已漂移（还写着 Codex cyan/magenta），随实现一并修正；上下文占用口径描述同步为「最近一次请求」。

---

## 四、方向契约（build 阶段植入桌面端根布局，逐字保留）

```
THESIS: Agent 会话即一夜天文观测；拒绝品类默认的「暗色+霓虹聊天皮肤」。
OWN-WORLD: 夜空 #0B1020 + 刻线图框 + 品牌蓝 #4D6BFE 星点 + 琥珀标注 +
mono 表格纪律；抽掉全部内容仍认得出是星图。
STORY: 开发者驱动 agent 观测其工作、在联锁前做出裁决、以测光复盘质量。
FIRST VIEWPORT: 左观测日志栏，顶观测之夜 runs 条带，中划线日志流 + ❯
composer，右星座图与六维测光表；主动作「新会话」居左栏顶。
FORM: 观测工作台（自列第 7 号方向，掷签指定，用户二轮确认）；seed 57f4f686。
FINISH: unreviewed and undocumented is unfinished; this build ends with
the finish review, the verdict, and DESIGN.md.
```

（注：本项目 DESIGN.md 为架构史文档，视觉系统记录落 `.impeccable/` 侧文档与本规范，不覆写架构 DESIGN.md。）

## 五、实现分期（供启动确认）

| 期 | 内容 | 验收 |
|---|---|---|
| P0 | 后端缺口：会话级 HTTP API + serve 本地认证 | 新端点集成测试 + `make check` + `make audit` |
| P1 | 重建 `crates/deepseeknova-desktop`（Tauri 壳 + sidecar serve + 工程栈） | 空窗可启动、连通 SSE |
| P2 | 主窗口：对话流 + composer + 侧栏 + runs 条带 | 与批准构图 B 逐区对照 |
| P3 | 星座图 + 测光评分卡 + 审批卡 | 签名交互（点星点跳条目）可用 |
| P4 | 归档/诊断/聚合/设置/onboarding + 印刷星图浅色档 | 全页面状态矩阵过检 |
| P5 | TUI 演进七项（§三） | 聚焦测试 + `make check` |
| P6 | finish review（detect.mjs + finish reviewer）+ 文档同步（GUIDE.md、AGENTS.md §2 crate 清单、BUILDING.md、README 截图） | 审查关闭、文档一致 |

> **P0 状态（2026-08-07 更新）**：已实现并冒烟通过。会话端点
> `GET/POST /v1/sessions`、`GET/DELETE /v1/sessions/{id}`、
> `POST /v1/sessions/{id}/chat`（SSE，busy 409，回合落盘口径与 TUI 一致）；
> `--token` 本地认证（`/health` 免认证）；CLI `serve` 分支已接线
> （session 目录 = `[session] root` 或 `~/.deepseeknova/sessions`，
> 与终端/桌面共享同一批会话）。验证：store 37 测试 + serve 22 集成
> 测试 + cli 测试通过；真实进程冒烟（认证 401/200、CRUD、chat 失败
> 回合不落盘）符合预期。待办：`make check` 全量 + `make audit`。

## 六、未决决策（实现前需用户拍板）

1. **界面文案语言**：现状中文；开源国际化通常英文优先。选项：i18n 双语（推荐，成本在 P2 就位最低）/ 全英 / 维持中文。
2. **Logo/应用图标**：无现存资产；星点/圆顶方向可另行立项，实现期先用文字标。
