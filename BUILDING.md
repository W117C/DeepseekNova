# DeepseekNova 编译与构建说明

本项目是纯 Rust 工作区（21 个 crate，无 GUI 桌面端），编译工作区或在 Linux 环境下运行本地开发测试时，按需安装系统原生库即可。

## Ubuntu / Debian 依赖安装

在编译前，请执行以下命令安装必要的 `pkg-config` 和常用原生库：

```bash
sudo apt update
sudo apt install -y \
  pkg-config \
  libssl-dev \
  libsqlite3-dev
```

## macOS 依赖安装

在 macOS 上编译通常不需要额外安装系统库，但需要 `pkg-config`：

```bash
brew install pkg-config
```

## Clone 后的 Git 配置（rename 跟踪）

项目历史上经历过多次重命名（`reasonix` → `DPronix` → `DeepNova` → `DeepseekNova`，命名现状的权威说明见 `DESIGN.md` “命名现状”一节）。为使依赖 Git 历史的统计分析（如热点分析）能合并更名前后的路径，clone 后请在仓库内执行一次：

```bash
git config log.follow true    # git log -- <path> 自动跟随重命名
git config diff.renames true  # diff/log 统计启用重命名检测
```

## 常用开发命令

我们提供了一个统一的 `Makefile` 以便于本地日常开发与 CI 对齐校验：

- **运行代码检查、测试与文档生成**：
  ```bash
  make check
  ```
  该命令会依次执行代码格式化检查、Clippy 静态分析（警告视作错误）、工作区内全部单元与集成测试、以及文档编译校验。

- **格式化代码**：
  ```bash
  make fmt
  ```

- **清理构建产物**：
  ```bash
  make clean
  ```

- **安全审计（依赖漏洞、许可证与冲突检查）**：
  ```bash
  make audit
  ```
  需预装 `cargo-deny`（`cargo install cargo-deny --locked`）；目标会先检查
  `cargo-deny`，再直接执行 `cargo deny --all-features check`。

- **同步 / 校验 README 测试数**：
  ```bash
  make test-count          # 在 Linux 上按 cargo test --all 的 passed 总数更新 README
  make test-count-check    # 校验 README 数字与 Linux CI passed 总数一致（非 Linux 本地跳过比对）
  ```
  数字由 `scripts/sync-test-count.py` 统一维护，避免徽章与表格各自漂移；
  权威口径为 Linux CI 的 passed 总数（`deepseeknova-sandbox` 含 Linux 专属
  测试，本地 macOS/Windows 运行结果可能更少），非 Linux 平台拒绝覆盖。

- **运行基准并保存基线（CI 对齐）**：
  ```bash
  make bench-ci
  ```
  运行工作区全部基准（core 事件/注册表/记忆检索、graph 解析/PageRank/检索），
  criterion 结果以命名基线 `ci` 保存到 `target/criterion`（内部 JSON：
  estimates.json 等），并打包到 `target/bench-ci/bench.tar.gz`。CI bench job
  复用本目标并上传产物为 artifact 供人工对比；未设自动门禁阈值（机器噪声
  易 flaky），性能退化由比对历史 artifact 发现。注意 criterion 0.8 已移除
  `--output-format json`，基线保存是现版本可用的 JSON 记录手段。

> 桌面端前端（`crates/deepseeknova-desktop`）已于 2026-08-08 整体移除，
> 历史可经 git 追溯（先例 `3ab55d7`）。当前无 Node 工程，本仓库为纯
> Rust workspace。
