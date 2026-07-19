# Security Policy

## Reporting a Vulnerability

If you discover a security vulnerability in DeepNova, please report it responsibly.

**Please do NOT open a public issue for security vulnerabilities.**

Instead, please report via one of the following channels:

1. **GitHub Security Advisory** (preferred):
   - Go to the [Security Advisories](https://github.com/W117C/DeepNova/security/advisories) page
   - Click "Report a vulnerability"
   - Fill in the details

2. **Email** (if unavailable via GitHub):
   - Send details to the repository maintainer

Please include in your report:
- Description of the vulnerability
- Steps to reproduce
- Affected versions
- Potential impact
- Suggested fix (if any)

## Response Process

| Stage | Timeline | Action |
|---|---|---|
| Acknowledgment | ≤ 48 hours | Confirm receipt and begin triage |
| Assessment | ≤ 7 days | Determine severity and impact |
| Fix & Release | ≤ 30 days (critical) | Develop patch, release fix |
| Disclosure | After fix public | Publish advisory with credit |

## Security Features

DeepNova implements defense-in-depth through multiple layers:

### Capability-Based Access Control
Tools must hold the required `Capability` to execute. Capabilities include:
- `FileRead` / `FileWrite` — filesystem access
- `CommandExecute` — shell execution
- `NetworkAccess` — outbound network calls
- `McpInvoke` — MCP server invocation
- `MemoryRead` / `MemoryWrite` — persistent memory access

Administrators can disable capabilities via `[security] disabled_capabilities` in `deepnova.toml`.

### Path Confinement
- Workspace root automatically added to allow-list
- `denied_paths` take precedence over all allow rules
- Tools attempting out-of-policy paths are blocked with audit logging

### Command & Domain Restrictions
- `allowed_commands` — restrict shell tool to specific command prefixes
- `allowed_domains` — restrict web_fetch to specific domains

### Resource Limits
Configurable limits prevent runaway execution:
- `max_files`, `max_file_size`, `max_total_read_bytes`
- `max_execution_time_secs`, `max_output_bytes`, `max_tool_calls`

### Audit Logging
All security-relevant events (capability violations, path denials) are logged via `TracingAuditLogger` to the tracing substrate.

## Hardening Recommendations

For production or multi-user deployments:

1. **Restrict capabilities** — disable any capability not actively needed:
   ```toml
   [security]
   disabled_capabilities = ["command_execute", "network_access"]
   ```

2. **Pin allowed paths** — explicitly list allowed directories:
   ```toml
   [security]
   allowed_paths = ["/data/project", "/data/build"]
   denied_paths   = ["/data/project/secrets"]
   ```

3. **Set resource limits** — cap resource consumption:
   ```toml
   [security.limits]
   max_files               = 100
   max_execution_time_secs = 60
   max_tool_calls          = 50
   ```

4. **Restrict commands** — limit shell to safe commands:
   ```toml
   [security]
   allowed_commands = ["git", "cargo", "ls"]
   ```

5. **Enable audit logging** — subscribe to tracing events to monitor security events.

## Supported Versions

| Version | Security Fixes |
|---|---|
| 0.3.x | ✅ Current |
| 0.2.x | ⚠️ Backports for critical vulnerabilities |
| 0.1.x | ❌ Not supported — upgrade recommended |
