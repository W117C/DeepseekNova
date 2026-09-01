# DeepseekNova Windows 沙箱网络/文件写限制 设计稿 v0（P6 残留 · docs-first）

> 状态：**设计稿（未实现）**——P6 的 Job Object 进程树隔离已实现
> （`deepseeknova-sandbox::windows::JobSandbox`，CI windows-latest 验证）；
> 本文档覆盖残留项：**网络限制**与**文件系统写路径限制**。只定方案与验收
> 标准，不引入代码；实现须在 Windows 环境专项进行，并以 CI windows-latest
> 真实用例作验收证据（本地 macOS 不可验证，遵守诚实约束）。

## 1. 威胁模型与目标

- **目标**：agent 派生的工具进程（shell 命令、MCP server、编译器）不得访问
  白名单外的网络端点；不得写工作区外的可写位置（系统目录、用户主目录、
  其他盘符）。
- **非目标**：防内核级提权；防管理员权限的合法用户；跨会话持久对抗。
- **与 Job Object 的关系**：Job Object 只做进程树归属与资源上限，不提供
  网络过滤与 ACL——本文两能力需 OS 原语补齐。

## 2. 网络限制：两条路径

### 2.1 路径 W：WFP（Windows Filtering Platform）整网/端口过滤

- 原理：BFE（Base Filtering Engine）服务 + WFP `FwpmEngineOpen` 注册
  `FWPM_LAYER_ALE_AUTH_CONNECT_V4/V6` 的 **BLOCK** filter（或按 app id /
  user SID 作用域的 permit 白名单）。
- 作用域建议：**按 user SID / app path 过滤**（`FWPM_CONDITION_ALE_APP_ID`
  精确到工具进程镜像路径），避免全机断网。
- 硬约束（设计上必须显式接受）：
  1. 需要**管理员**权限调用 `FwpmEngineOpen`（或预装 persist filter 的
     服务）——CLI 以普通用户运行时不可用，必须走提权安装器或伴随服务；
  2. 依赖 BFE 服务运行（默认自启，但可被策略禁用）；
  3. 过滤器泄漏风险：异常退出必须 `FwpmFilterDeleteById` 清理，且用
     flags 标注非持久（`FWPM_FILTER_FLAG_BOOTTIME` 不可滥用）。
- **结论**：v0 不默认启用；作为 opt-in 伴随服务路线（安装期注册按镜像
  路径的 BLOCK 白名单组）。

### 2.2 路径 A：AppContainer 低特权令牌（推荐 v0 首选）

- 原理：`DeriveRestrictedAppContainerSidFromAppContainerSid` + capability
  SID 集合构造低特权 token，子进程以该 token 创建（替代/叠加 Job Object）。
- 网络语义：**无 `INTERNET_CLIENT` capability 即默认无出网能力**——比 WFP
  简单且无需管理员；域白名单通过 `CheckNetIsolation` / package profile
  的能力声明做不到 per-domain，但"禁网/全开"两档可覆盖
  `NetworkPolicy` 的现有类型（域名级过滤仍属后续，与 seatbelt/bwrap 同限）。
- 文件写语义：进程默认对用户配置外位置**无写权限**；工作区目录需显式
  ACL 授予（`GrantAppContainerNamedObjectAccess` / 目录 DACL 追加
  container SID 的 write ACE）——精确实现工作区可写 = §1 目标的天然表达。
- 成本：profile 目录管理、SID/ACL 代码面较大；**无需管理员**是相对 WFP
  的决定性优势。

### 2.3 决策矩阵

| 维度 | W: WFP | A: AppContainer |
|------|--------|-----------------|
| 管理员要求 | 需要 | 不需要 |
| 禁网/全开 | 支持 | 支持 |
| 域名白名单 | 可做（IP 级） | 不支持（同 seatbelt 限制） |
| 文件写 ACL | 不相关（另行 DACL） | 原生融合 |
| 实现面 | 过滤器生命周期管理 | token/SID/ACL + profile |
| **v0 推荐** | opt-in 服务路线（后置） | **首选** |

## 3. 分层落地（AppContainer 首选路径）

1. **S1 禁网档**：`SandboxTier::WorkspaceWrite` + `NetworkPolicy::disabled`
   时，Windows 后端改用 AppContainer token 启动子进程（无 capability）。
   行为不变量：`curl example.com` 必失败；本机回环（IPC）不受影响需实测
   （loopback 由 `CheckNetIsolation LoopbackExempt` 独立管理，文档化）。
2. **S2 工作区可写**：以 container SID 对 workspace_root 及
   `.deepseeknova/` 授予 modify ACE；对 `%TEMP%` 子目录授予（编译器需要）。
   行为不变量：工作区内写成功；写 `%USERPROFILE%\Desktop\x.txt` 必失败。
3. **S3 回归网**：`NetworkPolicy::allow_all` 回退 Job Object 现状（能力
   协商：AppContainer 创建失败时 fail-closed 还是降级——**fail-closed**，
   与 permission 层语义对齐，降级必须显式配置）。

## 4. 验收标准（CI windows-latest 可执行）

- S1：沙箱内 `Invoke-WebRequest`/`curl` 非零退出或超时；对照组（非沙箱）
  成功。两用例进 `windows.rs` `#[cfg(windows)]` 测试。
- S2：沙箱内写 workspace 文件成功；写 `%USERPROFILE%` 外文件失败（错误
  含 Access is denied）。
- S3：`allow_all` 下网络用例成功；AppContainer 创建失败时 run 返回明确
  错误文本（含 "AppContainer unavailable"），**不静默降级**。
- 全部用例必须容忍 CI 环境差异（如 Hyper-V 隔离容器无 WFP）——skip 策略
  显式声明并记录原因，不允许静默 skip。

## 5. 开放问题

1. AppContainer 内运行 MSVC 工具链（link.exe 等）是否受 profile 目录限制
   影响增量构建？（需实测；必要时 profile 共享只读 + 本地临时可写）
2. MCP server 子进程族是否全部继承 container token？（Job Object + token
   叠加的继承语义需在 S1 用例中一并验证）
3. WFP 服务路线的产品形态（MSI 安装器？Windows 服务？）超出 CLI 范畴，
   需要用户决策后才排期。
