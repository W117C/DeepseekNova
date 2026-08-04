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