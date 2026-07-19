# GitHub 仓库设置指引

> 本指引用于在 [github.com/W117C/DeepseekNova](https://github.com/W117C/DeepseekNova) 上配置分支保护、Secrets 等安全设置。
> 由于当前环境无 `gh` CLI，请通过 Web UI 操作。

---

## 1. 创建 PR

浏览器打开以下链接，GitHub 会自动识别分支并提示创建 PR：

```
https://github.com/W117C/DeepseekNova/compare/main...fix/ci-clippy-warnings
```

或访问仓库页面 → **Pull requests** → **New pull request** → 选择 `fix/ci-clippy-warnings` → `main`。

**建议 PR 标题**：
```
feat(security): inject configurable security policy into tool execution
```

**建议 PR 描述**（可直接复制）：
```markdown
## Summary
- Fixes blocking bug: production `ToolContext` creation now injects `SecurityContext` + `workspace_root`, resolving the "SecurityContext extension not found" crash
- Adds configurable `[security]` section to `deepseeknova.toml` (capabilities, paths, commands, domains, resource limits)
- Adds `deepseeknova-security` crate with capability-based access control, path confinement, command/domain restrictions, resource limits, and audit logging
- Adds 5 new security regression tests (total ~253 tests workspace-wide)
- Syncs docs: README Security section, CHANGELOG [0.3.0], SECURITY.md, CODEOWNERS

## Verification
- [x] `cargo clippy --workspace --exclude deepseeknova-desktop --all-targets -- -D warnings` → 0 warnings
- [x] `cargo test --workspace --exclude deepseeknova-desktop` → ~253 tests, 0 failures
- [x] `cargo fmt --all -- --check` → clean

## Test plan
- [ ] Review security policy defaults match deployment expectations
- [ ] Verify `[security]` config section parses correctly in `deepseeknova.toml`
- [ ] Confirm `build_security_context()` correctly assembles `SecurityContext` from config
- [ ] Check audit logging output via `TracingAuditLogger`
```

---

## 2. 分支保护规则（Branch Protection）

**路径**：Settings → Branches → Branch protection rules → **Add rule**

| 设置项 | 推荐值 | 说明 |
|---|---|---|
| Branch name pattern | `main` | 保护主分支 |
| Require a pull request before merging | ✅ | 强制 PR 合并 |
| Require approvals | 1+ | 至少 1 人审批 |
| Dismiss stale pull request approvals when new commits are pushed | ✅ | 新提交后重审 |
| Require status checks to pass before merging | ✅ | CI 必须通过 |
| Require branches to be up to date before merging | ✅ | 分支必须最新 |
| Status checks that are required | `clippy`、`test`、`fmt`、`cargo-audit`（如有） | 对应 CI job 名 |
| Require conversation resolution before merging | ✅ | 解决所有评论 |
| Include administrators | 可选 | 管理员也受限制 |
| Restrict who can push to matching branches | 可选 | 限制直接推送 |

**操作步骤**：
1. 打开 https://github.com/W117C/DeepseekNova/settings/branches
2. 点击 **Add rule**
3. Branch name pattern 填 `main`
4. 勾选上述选项
5. 点击 **Create**

---

## 3. Secrets 配置

**路径**：Settings → Secrets and variables → Actions → **New repository secret**

| Secret 名称 | 说明 | 是否必需 |
|---|---|---|
| `CARGO_REGISTRY_TOKEN` | crates.io 发布 token | 仅发布时需要 |
| `DOCKERHUB_USERNAME` | Docker Hub 用户名 | 仅容器化部署时需要 |
| `DOCKERHUB_TOKEN` | Docker Hub 访问 token | 仅容器化部署时需要 |

**操作步骤**：
1. 打开 https://github.com/W117C/DeepseekNova/settings/secrets/actions
2. 点击 **New repository secret**
3. 填写 Name 和 Secret
4. 点击 **Add secret**

---

## 4. Environments（可选）

**路径**：Settings → Environments → **New environment**

建议创建：
- `production`：部署环境保护规则
- `staging`：预发布环境

**操作步骤**：
1. 打开 https://github.com/W117C/DeepseekNova/settings/environments
2. 点击 **New environment**
3. 填写名称（如 `production`）
4. 配置 protection rules（required reviewers、wait timer 等）
5. 点击 **Save protection rules**

---

## 5. 安全扫描（Security）

**路径**：Security → 各子菜单

| 功能 | 路径 | 说明 |
|---|---|---|
| Security policy | Security → Security policy | 已创建 SECURITY.md |
| Code scanning | Security → Code scanning | 启用 CodeQL 或第三方扫描 |
| Dependabot alerts | Security → Dependabot alerts | 启用依赖漏洞告警 |
| Secret scanning | Security → Secret scanning | 启用密钥泄露扫描 |

**建议启用**：
- ✅ Dependabot alerts（依赖漏洞告警）
- ✅ Dependabot security updates（自动安全更新）
- ✅ Secret scanning（密钥泄露扫描）

---

## 6. 其他推荐设置

### 6.1 General
**路径**：Settings → General

| 设置项 | 推荐值 |
|---|---|
| Automatically delete head branches | ✅ |
| Default branch | `main` |
| Squash merging | 允许（保持历史整洁） |
| Merge commits | 允许 |

### 6.2 Actions 权限
**路径**：Settings → Actions → General

| 设置项 | 推荐值 |
|---|---|
| Actions permissions | All actions |
| Approval required for running workflows from forks | ✅（安全最佳实践） |

---

## 7. 验证清单

完成上述设置后，确认：

- [ ] PR 已创建并显示 "Checks passing"
- [ ] 分支保护规则生效（直接 push 到 main 被拒绝）
- [ ] CI 状态检查显示为 required
- [ ] Dependabot alerts 已启用
- [ ] SECURITY.md 在仓库根目录可见

---

## 8. 快速链接

| 页面 | 链接 |
|---|---|
| 创建 PR | https://github.com/W117C/DeepseekNova/compare/main...fix/ci-clippy-warnings |
| 分支保护 | https://github.com/W117C/DeepseekNova/settings/branches |
| Secrets | https://github.com/W117C/DeepseekNova/settings/secrets/actions |
| Environments | https://github.com/W117C/DeepseekNova/settings/environments |
| Security | https://github.com/W117C/DeepseekNova/settings/security |
| Actions | https://github.com/W117C/DeepseekNova/settings/actions |
