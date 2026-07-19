# DeepseekNova 编译与构建说明

本项目包含 **Tauri 桌面端** (`deepseeknova-desktop`)，因此在编译整个工作区或在 Linux 环境下运行本地开发测试时，需要安装相关的系统原生库。

## Ubuntu / Debian 依赖安装

在编译前，请执行以下命令安装必要的 `pkg-config` 和 GUI/WebKit 原生库：

```bash
sudo apt update
sudo apt install -y \
  pkg-config \
  libglib2.0-dev \
  libgtk-3-dev \
  libwebkit2gtk-4.1-dev \
  libayatana-appindicator3-dev \
  librsvg2-dev
```

## macOS 依赖安装

在 macOS 上编译通常不需要额外安装 GTK 依赖，但如果需要编译特定依赖绑定，建议安装 `pkg-config`：

```bash
brew install pkg-config
```

## 常用开发命令

我们提供了一个统一的 `Makefile` 以便于本地日常开发与 CI 对齐校验：

- **运行代码检查、测试与文档生成**：
  ```bash
  make check
  ```
  该命令会依次执行代码格式化检查、Clippy 静态分析（警告视作错误）、全量单元与集成测试、以及文档编译校验。

- **格式化代码**：
  ```bash
  make fmt
  ```

- **清理构建产物**：
  ```bash
  make clean
  ```
