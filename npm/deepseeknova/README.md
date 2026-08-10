# deepseeknova

DeepseekNova CLI 的 npm 安装包：`postinstall` 从 GitHub Releases 下载当前平台
的预编译二进制（macOS / Linux / Windows x64 + arm64），校验 SHA-256 后解压到
包内 `vendor/`，`bin` 直接转发命令。

## Install

```bash
npm install -g deepseeknova
deepseeknova --help
```

## Requirements

- Node.js ≥ 16（仅安装脚本需要；运行期使用包内原生二进制）。
- 安装依赖 `tar`（macOS/Linux）或 `powershell`（Windows），均为系统自带。

## Versioning

包版本与 GitHub Release 标签一致（`v<version>`）。发布尚未生成的版本会得到
明确的下载失败提示。
