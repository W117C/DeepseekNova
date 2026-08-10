//! # 只读命令分类器
//!
//! 判断一条 shell 命令是否只读（四层分类 + 逐 flag 白名单 + 注入检测）。
//! 设计对照 Claude Code 的 `readOnlyValidation.ts`：
//!
//! 1. `READONLY_COMMANDS` — 任意参数安全（`ls`/`cat`/`head`/...）
//! 2. `READONLY_NOARGS` — 零参数安全（`pwd`/`whoami`/...）
//! 3. `READONLY_EXACT` — 精确形式安全（`node -v`/`claude --help`）
//! 4. `COMMAND_ALLOWLIST` — 按子命令 + flag 白名单（git/gh/docker 只读子命令）
//!
//! 真正的注入向量（UNC/URL/SMB 路径形态、git 全局 `-c`/`--config-env`
//! 配置注入、git 格式串注入）归为 [`ReadOnlyKind::Dangerous`] 直接拒绝；
//! 普通链式/重定向/命令替换按 [`ReadOnlyKind::NotReadOnly`] 走权限流程，
//! 由权限门/用户审批决定是否执行。
//!
//! [`CommandAudit`] 是只读分类的可预览结构（exec 审计模式），与
//! [`classify_readonly`] 同源——一次判定同时给出分类与命中形态。

use serde::{Deserialize, Serialize};

/// 命令分类结果。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ReadOnlyKind {
    /// 只读命令（可安全免询问执行）。
    ReadOnly,
    /// 非只读命令（走正常权限流程）。
    NotReadOnly,
    /// 注入/危险命令（应直接拒绝，不得执行）。
    Dangerous,
}

/// 只读分类命中的形态（四层分类 + 危险预检 + 规则回退）。
///
/// 供 exec 审计预览展示"这条命令为什么被这样分类"；与 [`classify_readonly`]
/// 同源（分类器一次判定即同时给出分类与形态）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ReadonlyForm {
    /// 危险注入形态预检（UNC/URL/SMB 路径、`/dev/tcp` 伪设备、git 全局
    /// `-c`/`--config-env` 或 `%G`/`%x` 格式串注入）→ 直接拒绝。
    Dangerous,
    /// 第一层：任意参数安全（`ls`/`cat`/`grep`/`du`/...）。
    Allowlist,
    /// 第四层：子命令 + flag 白名单（git/gh/docker/find/tar/openssl/...）。
    SubcommandAllowlist,
    /// 第三层：精确形式安全（`node -v`/`date -u`/`hostname -f`/...）。
    Exact,
    /// 第二层：零参数安全（`pwd`/`whoami`/...）。
    NoArgs,
    /// 无白名单命中，走权限审批/规则流程。
    Fallback,
}

/// 只读分类的可预览结构（exec 审计模式）。
///
/// 由 CLI `audit` 子命令与 permission crate 的 `PermissionGate` 预览消费：
/// 命令 + 分类 + 是否免询问 + 命中形态说明。`from_command` 与
/// [`classify_readonly`] 同源（同一实现），保证预览分类与真实执行路径
/// 决策零漂移。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CommandAudit {
    /// 被分类的原始命令。
    pub command: String,
    /// 分类结果（只读 / 非只读 / 危险）。
    pub kind: ReadOnlyKind,
    /// 是否可免询问。分类层语义：`ReadOnly` 为 `true`；权限门仍可能因
    /// 显式 deny/ask 规则而询问或拒绝——最终裁决见 gate 预览。
    pub allow_without_prompt: bool,
    /// 命中的分类形态（四层 + 危险 + 回退）。
    pub form: ReadonlyForm,
    /// 人类可读的命中说明。
    pub explanation: String,
}

impl CommandAudit {
    /// 对一条 shell 命令做只读分类预览。
    ///
    /// 与真实执行路径 `classify_readonly` 同一实现，语义零漂移
    /// （一致性由测试 `command_audit_matches_classify_readonly` 保证）。
    pub fn from_command(cmd: impl Into<String>) -> Self {
        let command = cmd.into();
        let (kind, form) = classify_with_form(&command);
        let explanation = match form {
            ReadonlyForm::Dangerous => {
                "命中危险注入形态（UNC/URL/SMB 路径、/dev/tcp 伪设备、git 全局 \
                 -c/--config-env 或 %G/%x 格式串注入），直接拒绝"
            }
            ReadonlyForm::Allowlist => {
                "命中「任意参数安全」白名单（ls/cat/grep/du/...），免询问放行"
            }
            ReadonlyForm::SubcommandAllowlist => {
                "命中子命令 + flag 白名单（git/gh/docker/find/tar/openssl/... 只读形态），免询问放行"
            }
            ReadonlyForm::Exact => "精确形式安全（如 node -v / date -u），免询问放行",
            ReadonlyForm::NoArgs => "零参数安全（如 pwd/whoami），免询问放行",
            ReadonlyForm::Fallback => "无白名单命中，走权限审批/规则流程",
        };
        Self {
            allow_without_prompt: kind == ReadOnlyKind::ReadOnly,
            kind,
            form,
            command,
            explanation: explanation.to_string(),
        }
    }
}

// ---------------------------------------------------------------------------
// 第一层：任意参数安全的命令
// ---------------------------------------------------------------------------

// ---------------------------------------------------------------------------
// 第一层：任意参数安全的命令
// ---------------------------------------------------------------------------

/// 多词"任意参数安全"条目中**保持前缀匹配**的白名单（L5 例外清单）。
///
/// 仅当写形态是独立子命令/动词、尾部参数是只读过滤器时才允许前缀匹配：
/// `systemctl status sshd`、`log show --predicate ...`、`defaults read key`。
/// 这些命令的写操作（`systemctl start`、`log erase`、`defaults write`）是
/// 不同位置参数，前缀不可能命中；链式/重定向已在注入检测短路。
/// 其余多词条目一律**精确 argv 匹配**——`sysctl -a -w`、`ssh-keygen -y -f`、
/// `fc -l -s` 等尾部 flag 转写/泄密形态不得免询问逃逸。
const MULTIWORD_PREFIX_SAFE: &[&str] = &[
    "systemctl list-units",
    "systemctl status",
    "log show",
    "defaults read",
];

const READONLY_COMMANDS: &[&str] = &[
    "ls",
    "cat",
    "head",
    "tail",
    "less",
    "more",
    "wc",
    "grep",
    "ag",
    "locate",
    "which",
    "where",
    "type",
    "stat",
    "du",
    "df",
    "uname",
    "uname -a",
    "arch",
    "uptime",
    "who",
    "w",
    "id",
    "groups",
    "finger",
    "ps",
    "top",
    "htop",
    "lsof",
    "netstat",
    "ss",
    "nproc",
    "free",
    "vmstat",
    "iostat",
    "lsblk",
    "fc -l",
    "tree",
    "jq",
    "base64",
    "md5",
    "md5sum",
    "sha1sum",
    "sha256sum",
    "cksum",
    "hexdump",
    "xxd",
    "od",
    "strings",
    "nm",
    "objdump",
    "readelf",
    "ldd",
    "zipinfo",
    "openssl version",
    "ssh-keygen -l",
    "ssh-keygen -y",
    "fc-list",
    "fc-match",
    "locale",
    "locale -a",
    "timedatectl status",
    "systemctl list-units",
    "systemctl status",
    "log show",
    "sysctl -a",
    "defaults read",
    "pkgutil --pkg-info",
    "sw_vers",
    "system_profiler",
    "mdls",
    "mdfind",
];

// 注：以下命令**不可**进入"任意参数安全"表，均为专项白名单处理：
// `find`（-delete/-exec/-execdir 写）、`env`（程序执行器）、`xargs`（执行器）、
// `tar -t`（-txf 组合解包）、`openssl x509`（-req/-out 写证书）、
// `xattr`（-w/-c/-d 写属性）、`gpg --list-keys`（--export-secret-keys 泄私钥）、
// `journalctl`（--vacuum-time 删日志）、`plutil -p`（-convert 组合）、`gh auth status`、
// `unzip -l`（-d/-o 解包写文件）、`7z l`（-o 输出目录写文件）
// （L5：凡可因尾部 flag 转写操作的多词条目一律精确 argv 匹配或专项白名单）

/// 零参数安全的命令。
const READONLY_NOARGS: &[&str] = &[
    "pwd", "whoami", "id -un", "umask", "tty", "logname", "users", "w", "who am i", "last",
    "history",
];

/// 精确形式安全（命令 + 固定参数）。
const READONLY_EXACT: &[(&str, &str)] = &[
    ("node", "-v"),
    ("node", "--version"),
    ("npm", "-v"),
    ("npm", "--version"),
    ("python", "--version"),
    ("python3", "--version"),
    ("python3", "-V"),
    ("claude", "--help"),
    ("claude", "--version"),
    ("cargo", "--version"),
    ("cargo", "-V"),
    ("rustc", "--version"),
    ("rustup", "--version"),
    ("git", "--version"),
    ("git", "--help"),
    ("docker", "--version"),
    ("gh", "--version"),
    ("curl", "--version"),
    ("wget", "--version"),
    // `date`/`hostname` 的固定形态：带位置参数或 `-s`/`-f` 值会变写操作，
    // 不能进"任意参数安全"表，只能精确匹配。
    ("date", "-u"),
    ("date", "+%s"),
    ("hostname", "-f"),
    ("hostname", "-s"),
];

// ---------------------------------------------------------------------------
// Allowlist 层：子命令 + flag 白名单
// ---------------------------------------------------------------------------

/// 子命令规格：只读子命令允许的 flag 与位置参数约束。
#[derive(Clone, Copy)]
struct SubcommandSpec {
    name: &'static str,
    /// 允许的短 flag 字符集合（如 "a" 表示 -a）。
    short_ok: &'static str,
    /// 允许的长 flag 前缀（`--name` 或 `--name=value`）。
    long_ok: &'static [&'static str],
    /// 出现即拒的长 flag 精确名。
    long_deny: &'static [&'static str],
    /// 位置参数上限（None = 任意）。
    positional_max: Option<usize>,
}

const GIT_READONLY: &[SubcommandSpec] = &[
    SubcommandSpec {
        name: "status",
        short_ok: "sb",
        long_ok: &[
            "--porcelain",
            "--short",
            "--branch",
            "--untracked-files",
            "--ignored",
        ],
        long_deny: &[],
        positional_max: Some(1),
    },
    SubcommandSpec {
        name: "diff",
        short_ok: "p",
        long_ok: &[
            "--stat",
            "--name-only",
            "--name-status",
            "--cached",
            "--staged",
            "--no-index",
            "--word-diff",
            "--numstat",
            "--shortstat",
            "--summary",
            "--check",
            "--color",
            "--no-color",
        ],
        long_deny: &["--output", "--format", "--pretty"],
        positional_max: None,
    },
    SubcommandSpec {
        name: "log",
        short_ok: "pc", // -c = combined diff（只读）；-c 注入面仅限全局区，已单独拦截
        long_ok: &[
            "--oneline",
            "--stat",
            "--name-only",
            "--name-status",
            "--graph",
            "--decorate",
            "--all",
            "--author",
            "--since",
            "--until",
            "--grep",
            "--max-count",
            "--reverse",
            "--no-pager",
            "--color",
            "--no-color",
            "--numstat",
            "--shortstat",
            "--format",
            "--pretty",
            "--date",
            "--abbrev-commit",
            "--relative-date",
            "--topo-order",
            "--ancestry-path",
        ],
        long_deny: &["--output"],
        positional_max: None,
    },
    SubcommandSpec {
        name: "show",
        short_ok: "p",
        long_ok: &[
            "--stat",
            "--name-only",
            "--name-status",
            "--no-pager",
            "--format",
            "--pretty",
            "--color",
            "--no-color",
        ],
        long_deny: &["--output"],
        positional_max: None,
    },
    SubcommandSpec {
        name: "rev-parse",
        short_ok: "",
        long_ok: &[
            "--abbrev-ref",
            "--show-toplevel",
            "--show-prefix",
            "--is-inside-work-tree",
            "--is-inside-git-dir",
            "--verify",
            "--short",
            "--symbolic-full-name",
            "--git-dir",
        ],
        long_deny: &[],
        positional_max: None,
    },
    SubcommandSpec {
        name: "ls-files",
        short_ok: "",
        long_ok: &[
            "--cached",
            "--modified",
            "--others",
            "--deleted",
            "--stage",
            "--error-unmatch",
        ],
        long_deny: &[],
        positional_max: None,
    },
    SubcommandSpec {
        name: "ls-tree",
        short_ok: "r",
        long_ok: &["--name-only", "--name-status", "--full-tree", "--recursive"],
        long_deny: &[],
        positional_max: None,
    },
    SubcommandSpec {
        name: "grep",
        short_ok: "niw",
        long_ok: &[
            "--line-number",
            "--ignore-case",
            "--word-regexp",
            "--no-color",
            "--break",
            "--heading",
            "--cached",
            "--untracked",
        ],
        long_deny: &[],
        positional_max: None,
    },
    SubcommandSpec {
        name: "blame",
        short_ok: "",
        long_ok: &["--line-porcelain", "--show-email"],
        long_deny: &[],
        positional_max: None,
    },
    SubcommandSpec {
        name: "remote",
        short_ok: "v",
        long_ok: &[],
        long_deny: &[],
        positional_max: Some(1),
    },
    SubcommandSpec {
        name: "help",
        short_ok: "",
        long_ok: &[],
        long_deny: &[],
        positional_max: None,
    },
    SubcommandSpec {
        name: "diff-tree",
        short_ok: "",
        long_ok: &["--name-only", "--name-status", "--stat"],
        long_deny: &["--output"],
        positional_max: None,
    },
    SubcommandSpec {
        name: "diff-index",
        short_ok: "",
        long_ok: &["--name-only", "--name-status", "--cached", "--stat"],
        long_deny: &["--output"],
        positional_max: None,
    },
    SubcommandSpec {
        name: "diff-files",
        short_ok: "",
        long_ok: &["--name-only", "--name-status", "--stat", "--cached"],
        long_deny: &["--output"],
        positional_max: None,
    },
    SubcommandSpec {
        name: "describe",
        short_ok: "",
        long_ok: &["--tags", "--abbrev", "--always", "--dirty"],
        long_deny: &[],
        positional_max: None,
    },
    SubcommandSpec {
        name: "name-rev",
        short_ok: "",
        long_ok: &["--tags", "--refs"],
        long_deny: &[],
        positional_max: None,
    },
    SubcommandSpec {
        name: "check-ignore",
        short_ok: "v",
        long_ok: &["--verbose", "--stdin"],
        long_deny: &[],
        positional_max: None,
    },
    SubcommandSpec {
        name: "check-attr",
        short_ok: "",
        long_ok: &["--stdin", "--all", "--cached"],
        long_deny: &[],
        positional_max: None,
    },
    SubcommandSpec {
        name: "submodule",
        short_ok: "",
        long_ok: &[],
        long_deny: &[],
        // submodule 是"子命令 + 位置参数"形态：`foreach` 在子模块内执行
        // 任意 shell 命令（执行器，与 find -exec 同型）、`update`/`deinit`/
        // `add`/`absorbgitdirs` 是写操作。仅 `status` 只读：位置参数
        // 精确匹配 + 无 flag。由 submodule_allowed 专项处理。
        positional_max: Some(0),
    },
];

/// git submodule 只读判定：仅 `status`（可带子模块路径，只读）。
/// `foreach` 在子模块内执行任意 shell 命令（执行器，与 find -exec 同型）、
/// `update`/`deinit`/`add`/`absorbgitdirs` 等是写操作，一律拒绝。
fn git_submodule_allowed(args: &[String]) -> bool {
    let Some(action) = args.first() else {
        return false; // 裸 `git submodule` 无意义，保守拒
    };
    if action != "status" {
        return false;
    }
    // 其余位置参数是子模块路径（只读查询目标），flag 仅允许 --recursive
    args.iter()
        .skip(1)
        .all(|a| !a.starts_with('-') || a == "--recursive")
}

/// git branch 只读判定：位置参数是分支名（创建/删除目标）。
/// 仅显式携带读 flag（-a/-r/-v/--list/--show-current/--merged/--no-merged/
/// -vv 等）且无裸位置参数（--list 后跟 pattern 允许）时放行。
fn git_branch_allowed(args: &[String]) -> bool {
    const READ_FLAGS: &[&str] = &[
        "-a",
        "-r",
        "-v",
        "-vv",
        "--list",
        "--show-current",
        "--merged",
        "--no-merged",
        "--no-color",
        "--format",
        "--sort",
    ];
    let mut has_read_flag = false;
    for a in args {
        if a.starts_with('-') {
            if a == "--" {
                continue;
            }
            if matches!(
                a.as_str(),
                "--list" | "--show-current" | "-a" | "-r" | "-v" | "-vv"
            ) {
                has_read_flag = true;
            }
            if !READ_FLAGS.contains(&a.as_str()) {
                return false;
            }
        } else {
            // 位置参数仅当存在读 flag 时允许（pattern 匹配）
            if !has_read_flag {
                return false;
            }
        }
    }
    has_read_flag
}

/// git tag 只读判定：位置参数是标签名（创建/删除目标）。
/// 仅显式携带读 flag（-l/-n/--list/--contains/--sort/--no-color）时放行。
fn git_tag_allowed(args: &[String]) -> bool {
    const READ_FLAGS: &[&str] = &[
        "-l",
        "-n",
        "--list",
        "--contains",
        "--sort",
        "--no-color",
        "--points-at",
    ];
    let mut has_read_flag = false;
    for a in args {
        if a.starts_with('-') {
            if a == "--" {
                continue;
            }
            if a == "-l" || a == "--list" {
                has_read_flag = true;
            }
            if !READ_FLAGS.contains(&a.as_str()) {
                return false;
            }
        } else if !has_read_flag {
            return false; // 无读 flag 时的位置参数 = 创建标签
        }
    }
    has_read_flag
}

/// git stash 特殊处理：`stash list`/`stash show` 只读；
/// `pop`/`apply`/`drop`/`clear`/`push` 等都会改动工作区，一律拒绝。
/// 裸 `git stash`（等价 push）与任何 flag（`-u`/`-a`/`-p`/`-m` 均为创建
/// stash 的写操作）同样拒绝。
fn git_stash_allowed(args: &[String]) -> bool {
    // 位置参数必须是 list 或 show（`show` 可带一个 stash 引用）
    let positional: Vec<&String> = args.iter().filter(|a| !a.starts_with('-')).collect();
    if positional.is_empty() {
        return false; // 裸 `git stash` = push（写操作）
    }
    match positional[0].as_str() {
        "list" => positional.len() == 1 && args.iter().all(|a| !a.starts_with('-')),
        "show" => {
            positional.len() <= 2
                && args
                    .iter()
                    .all(|a| !a.starts_with('-') || a == "--stat" || a == "--patch")
        }
        _ => false,
    }
}

const GH_READONLY_SUBCOMMANDS: &[&str] = &[
    "auth status",
    "repo view",
    "pr view",
    "pr list",
    "pr status",
    "pr diff",
    "issue view",
    "issue list",
    "gist view",
    "gist list",
    "search repos",
    "search issues",
    "search prs",
    "search code",
    "search commits",
    "api",
    "config get",
    "help",
    "version",
];

const DOCKER_READONLY_SUBCOMMANDS: &[&str] = &[
    "ps",
    "images",
    "inspect",
    "logs",
    "info",
    "version",
    "network ls",
    "volume ls",
    "events",
    "top",
    "stats",
    "port",
];

// ---------------------------------------------------------------------------
// Tokenizer（仅用于分类，不执行）
// ---------------------------------------------------------------------------

/// 按 shell 词法切分命令为 argv。单双引号与反斜杠转义感知。
///
/// 返回 `(args, closed, injected)`：
/// - `closed=false`：引号未闭合——命令无法正常执行，分类器保守拒绝
/// - `injected=true`：任一 token 含**未引用**的链式/重定向符号（`|` `;` `&` `>`
///   `<`），或含命令替换/参数展开（`$(` `${` `` ` ``——引号内同样执行），
///   或含未引用换行。此类命令归 [`ReadOnlyKind::NotReadOnly`] 走权限流程，
///   但**不得**被任意参数安全表放行（`ls $(rm -rf /)` 仍是执行链）。
///
/// 引号内（或转义后）的 `|`/`;` 等是字面量（`grep -E 'a|b'`、`sed 's/|//'`），
/// 不构成链式，注入检测必须感知引用状态，不能对原始串做子串扫描。
pub(crate) fn split_args(cmd: &str) -> (Vec<String>, bool, bool) {
    let mut args = Vec::new();
    let mut cur = String::new();
    let mut in_single = false;
    let mut in_double = false;
    let mut injected = false;
    // 当前 token 是否含未引用链式符号（`|` `;` `&` `>` `<`）
    let mut cur_unquoted_inject = false;
    let mut chars = cmd.chars().peekable();
    while let Some(c) = chars.next() {
        match c {
            '\'' if !in_double => in_single = !in_single,
            '"' if !in_single => in_double = !in_double,
            '\\' if !in_single => {
                if let Some(n) = chars.next() {
                    cur.push(n); // 转义后的字符为字面量，不触发注入检测
                }
            }
            c if c.is_whitespace() && !in_single && !in_double => {
                if c == '\n' {
                    // 未引用换行 = 新命令边界 → 归 NotReadOnly（不得进只读表）
                    injected = true;
                }
                if !cur.is_empty() {
                    args.push(std::mem::take(&mut cur));
                    if cur_unquoted_inject {
                        injected = true;
                    }
                    cur_unquoted_inject = false;
                }
            }
            c => {
                // 命令替换/参数展开：引号内外都执行，归 NotReadOnly
                if c == '$' {
                    if let Some(&next) = chars.peek() {
                        if next == '(' || next == '{' {
                            cur_unquoted_inject = true; // 由 token 收口置 injected
                            cur.push(c);
                            cur.push(next);
                            chars.next();
                            continue;
                        }
                    }
                }
                if c == '`' {
                    cur_unquoted_inject = true;
                    cur.push(c);
                    continue;
                }
                // 链式/重定向符号：仅在未引用时命中
                if !in_single && !in_double && matches!(c, '|' | ';' | '&' | '>' | '<') {
                    cur_unquoted_inject = true;
                }
                cur.push(c);
            }
        }
    }
    if !cur.is_empty() {
        args.push(cur);
        if cur_unquoted_inject {
            injected = true;
        }
    }
    (args, !in_single && !in_double, injected)
}

// ---------------------------------------------------------------------------
// 注入检测
// ---------------------------------------------------------------------------

/// 危险路径形式：UNC / URL / SMB 注入（与 token 引用状态无关的全局形态）。
fn has_dangerous_path_form(cmd: &str) -> bool {
    if cmd.contains("@SSL@") || cmd.contains("DavWWWRoot") {
        return true;
    }
    // `/dev/tcp/`、`/dev/udp/` 伪设备：bash 视为网络重定向（读任意端口/本地
    // 服务、注入云元数据上下文）。即使 `cat` 等只读命令携带同样构成远程访问
    // 面 → 硬拒；`echo hi > /dev/tcp/x/80` 的写形态也经此预检先于 injected
    // 判定被拒绝。`/dev/tcp/*` 等变体同样含 `/dev/tcp/` 命中。
    if cmd.contains("/dev/tcp/") || cmd.contains("/dev/udp/") {
        return true;
    }
    // `//` 前缀的 UNC 路径（`//evil/share`）
    let trimmed = cmd.trim_start();
    trimmed.starts_with("//")
}

/// git 格式串注入检测：`--format`/`--pretty`/`--sort` 值中仅拒绝危险模式。
/// 同时覆盖 `--format=value`（单 token）与 `--format value`（分离值）两种形态。
///
/// - `%G`/`%g`（GPG 签名验证，可触发凭据钩子）
/// - `%x`（十六进制转义，可注入 ANSI 控制序列）
///
/// `format:` 前缀本身（`--pretty=format:%s`）是 git 的常规格式语法，
/// 不构成注入；`%h`/`%s`/`%d` 等常规格式符同样不受影响。
fn git_format_injection(args: &[String]) -> bool {
    let format_flags = ["--format", "--pretty", "--sort"];
    for (i, a) in args.iter().enumerate() {
        for flag in format_flags {
            let value = if let Some(v) = a.strip_prefix(&format!("{flag}=")) {
                Some(v)
            } else if a == flag {
                // 分离值形态：`--format '%G?'`——取下一个 token 当值
                args.get(i + 1).map(|s| s.as_str())
            } else {
                continue;
            };
            let Some(value) = value else { continue };
            if value.contains("%G") || value.contains("%g") {
                return true;
            }
            // %x 十六进制转义（ANSI 注入面）
            let bytes: Vec<char> = value.chars().collect();
            for i in 0..bytes.len().saturating_sub(1) {
                if bytes[i] == '%' && bytes[i + 1] == 'x' {
                    return true;
                }
            }
        }
    }
    false
}

// ---------------------------------------------------------------------------
// Allowlist 判定
// ---------------------------------------------------------------------------

/// 检查 argv 中每个 flag 是否命中白名单/黑名单。
/// `short_ok` 为允许的短 flag 字符集（`-abc` 逐字符校验）；
/// 长 flag 支持 `--name` 与 `--name=value` 两种形式。
fn flags_ok(args: &[String], spec: &SubcommandSpec) -> bool {
    let mut positional = 0usize;
    for a in args {
        if a == "--" {
            // 显式结束符后全部为位置参数
            positional += 1;
            continue;
        }
        if let Some(long) = a.strip_prefix("--") {
            if spec
                .long_deny
                .iter()
                .any(|d| *d == long || long.starts_with(&format!("{d}=")))
            {
                return false;
            }
            let name = long.split('=').next().unwrap_or(long);
            // long_ok 表项带 `--` 前缀，name 已去前缀
            if !spec
                .long_ok
                .iter()
                .any(|ok| ok.strip_prefix("--") == Some(name))
            {
                return false;
            }
        } else if let Some(short) = a.strip_prefix('-') {
            if short.is_empty() || short == "-" {
                continue; // "-" 与裸参数
            }
            if short.starts_with('-') {
                return false; // 形式异常，保守拒绝
            }
            // 短 flag 组合逐字符校验；`-5`/`-12` 数字形式（git 数量限制）放行
            if short.chars().all(|c| c.is_ascii_digit()) {
                continue;
            }
            if !short.chars().all(|c| spec.short_ok.contains(c)) {
                return false;
            }
        } else {
            positional += 1;
            if let Some(max) = spec.positional_max {
                if positional > max {
                    return false;
                }
            }
        }
    }
    true
}

/// git 只读判定：先排除全局 `-c` 注入与格式串注入，再按子命令查表。
fn git_allowed(args: &[String]) -> bool {
    // 跳过 git 全局 flag 并检查配置注入。
    // 注意：`--version`/`--help`/`--no-pager` 不带参数（只跳 1）；
    // `-C` 带一个目录参数（跳 2）。`-c key=value` / `--config-env`
    // 可注入 core.pager 等执行任意命令——只检查**子命令之前**的全局区，
    // 子命令后的 `-c` 是合法只读 flag（如 `git log -c` combined diff）。
    let mut i = 1;
    loop {
        let Some(a) = args.get(i) else {
            return false; // 只有全局 flag：保守拒（信息不足）
        };
        if a == "--version" || a == "--help" || a == "--no-pager" {
            i += 1;
            continue;
        }
        if a == "-C" {
            i += 2;
            continue;
        }
        if a.starts_with("-c") || a.starts_with("--config-env") {
            return false;
        }
        break;
    }
    if git_format_injection(args) {
        return false;
    }

    let Some(sub) = args.get(i) else {
        return false;
    };

    if *sub == "config" {
        return git_config_allowed(&args[i + 1..]);
    }
    if *sub == "stash" {
        return git_stash_allowed(&args[i + 1..]);
    }
    if *sub == "submodule" {
        return git_submodule_allowed(&args[i + 1..]);
    }
    // branch/tag：位置参数 = 创建/删除目标（`git branch foo` / `git tag v1`
    // 是写操作）。仅显式读 flag（--list/--show-current/-a/-r/-v/-l/-n）放行。
    if *sub == "branch" {
        return git_branch_allowed(&args[i + 1..]);
    }
    if *sub == "tag" {
        return git_tag_allowed(&args[i + 1..]);
    }

    // `git branch -D` 之类：-D/-d/-m/-M 都是写操作，短 flag 白名单已覆盖
    let spec = GIT_READONLY.iter().find(|s| s.name == sub).copied();
    match spec {
        Some(s) => flags_ok(&args[i + 1..], &s),
        None => false,
    }
}

/// git config 特殊处理：裸 `git config key value` 是**写**操作，
/// 必须显式带 `--get`/`--list`/`--get-regexp` 之一才视为只读。
fn git_config_allowed(args: &[String]) -> bool {
    let has_read_flag = args
        .iter()
        .any(|a| a == "--get" || a == "--list" || a == "--get-regexp");
    if !has_read_flag {
        return false;
    }
    // 其余 flag 仍须在白名单内
    let spec = SubcommandSpec {
        name: "config",
        short_ok: "",
        long_ok: &[
            "--get",
            "--list",
            "--global",
            "--local",
            "--system",
            "--get-regexp",
            "--show-origin",
            "--null",
        ],
        long_deny: &[],
        positional_max: Some(2),
    };
    flags_ok(args, &spec)
}

/// find 只读判定：读 flag 白名单，`-delete`/`-exec`/`-execdir`/`-ok` 等
/// 写操作显式拒绝（任意参数表不可容纳 find——`-exec` 是任意命令执行器）。
fn find_allowed(args: &[String]) -> bool {
    // 出现即拒的写/执行 flag
    const WRITE_FLAGS: &[&str] = &[
        "-delete", "-exec", "-execdir", "-ok", "-okdir", "-fprint", "-fprintf", "-fprint0", "-fls",
        "-touch",
    ];
    // 允许的读 flag（带值 flag 的值是位置参数，单独计数）
    const READ_FLAGS: &[&str] = &[
        "-name",
        "-iname",
        "-path",
        "-ipath",
        "-type",
        "-maxdepth",
        "-mindepth",
        "-print",
        "-print0",
        "-ls",
        "-size",
        "-mtime",
        "-atime",
        "-ctime",
        "-newer",
        "-user",
        "-group",
        "-perm",
        "-not",
        "-and",
        "-or",
        "-true",
        "-false",
        "-empty",
        "-links",
        "-inum",
        "-regex",
        "-iregex",
        "-quit",
        "-noleaf",
        "-depth",
        "-follow",
        "-xdev",
        "-mount",
        "-prune",
        "-daystart",
        "-amin",
        "-cmin",
        "-mmin",
        "-anewer",
        "-cnewer",
        "-used",
        "-nouser",
        "-nogroup",
        "-nofollow",
        "-regextype",
        "-printf",
    ];
    for a in args {
        if a.starts_with('-') {
            if WRITE_FLAGS.contains(&a.as_str()) {
                return false;
            }
            // `-newerXY`（X/Y ∈ aBcCmt）：只读时间比较，形如 `-newermt`（8 字节）
            if a.starts_with("-newer")
                && a.len() == 8
                && a.as_bytes()[6..].iter().all(|b| b"aBcCmt".contains(b))
            {
                continue;
            }
            if !READ_FLAGS.contains(&a.as_str()) {
                return false;
            }
        }
    }
    true
}

/// tar 只读判定：仅 `-t`（list）组合，且**全部**短 flag 组合中不得含
/// `x`（解包）/`c`（创建）/`r`（append）/`u`（update）。`-txf x.tar`
/// 解包写文件必须拒绝；`-tvf`/`-tzf` 只读放行。
///
/// 执行类长 flag（GNU tar）：`--checkpoint-action=exec`（读路径也触发
/// RCE）、`--to-command`、`-F/--info-script`、`--rmt-command`、
/// `--use-compress-program` 一律拒绝；`--checkpoint` 仅允许不带 action。
fn tar_allowed(args: &[String]) -> bool {
    let mut list_seen = false;
    for a in args {
        if a.starts_with("--") {
            // 长 flag：仅 --checkpoint（无 action）/--warning/--ignore-zeros 等
            // 纯展示类放行；执行类与写类拒绝
            let name = a.split('=').next().unwrap_or(a);
            match name {
                "--checkpoint" => {
                    if a.contains('=') && !a.starts_with("--checkpoint=") {
                        return false;
                    }
                    // 有 --checkpoint-action 时拒绝（在下方单独检测）
                }
                "--checkpoint-action"
                | "--to-command"
                | "--info-script"
                | "--rmt-command"
                | "--use-compress-program"
                | "--extract"
                | "--create"
                | "--append"
                | "--update"
                | "--delete" => return false,
                _ => {
                    // 其余长 flag 保守拒绝（--list/--file 等虽只读，但
                    // 长 flag 组合面大，宁走权限流程）
                    return false;
                }
            }
        } else if a.starts_with('-') && a.len() > 1 {
            let body = &a[1..];
            for c in body.chars() {
                match c {
                    't' => list_seen = true,
                    'x' | 'c' | 'r' | 'u' | 'F' => return false, // 解包/创建/追加/更新/脚本
                    _ => {}
                }
            }
        } else {
            // 位置参数（归档文件）
        }
    }
    if !list_seen {
        return false; // 非 list 操作
    }
    // 位置参数（归档文件）上限 1
    let positional = args.iter().filter(|a| !a.starts_with('-')).count();
    positional <= 1
}

/// rg 只读判定：ripgrep 本身是纯搜索、不写文件，但 `--pre`/`--pre-glob`
/// 会在每个匹配文件上执行外部命令（命令执行器，归档的"执行器混入任意
/// 参数安全表"漏洞类）→ 拒绝，归 NotReadOnly 走权限流程。
fn rg_allowed(args: &[String]) -> bool {
    !args.iter().any(|a| {
        a == "--pre" || a.starts_with("--pre=") || a == "--pre-glob" || a.starts_with("--pre-glob=")
    })
}

/// yq 只读判定：读模式安全；`-i`/`--inplace`（mikefarah/yq 与 kislyuk/yq
/// 均支持）就地写文件 → 拒绝，归 NotReadOnly 走权限流程。
fn yq_allowed(args: &[String]) -> bool {
    !args.iter().any(|a| a == "-i" || a == "--inplace")
}

/// openssl x509 只读判定：仅证书查看 flag；`-req`（签名请求）/`-out`
/// （写文件）/`-signkey` 等写操作拒绝。
fn openssl_x509_allowed(args: &[String]) -> bool {
    const READ_FLAGS: &[&str] = &[
        "-in",
        "-text",
        "-noout",
        "-subject",
        "-issuer",
        "-dates",
        "-fingerprint",
        "-serial",
        "-enddate",
        "-hash",
        "-modulus",
        "-pubkey",
        "-inform",
        "-infile",
        "-outform",
    ];
    const WRITE_FLAGS: &[&str] = &[
        "-req",
        "-out",
        "-signkey",
        "-new",
        "-newkey",
        "-set_serial",
        "-days",
        "-x509toreq",
        "-extfile",
        "-extensions",
        "-config",
        "-passin",
        "-passout",
        "-force_pubkey",
    ];
    let mut positional = 0usize;
    for a in args {
        if a.starts_with('-') {
            if WRITE_FLAGS.contains(&a.as_str()) {
                return false;
            }
            if !READ_FLAGS.contains(&a.as_str()) {
                return false;
            }
        } else {
            positional += 1;
            if positional > 1 {
                return false;
            }
        }
    }
    true
}

/// xattr 只读判定：仅 `-l`（list）/`-p`（print）/无 flag；`-w`/`-c`/`-d`
/// 写/清/删扩展属性，拒绝。
fn xattr_allowed(args: &[String]) -> bool {
    for a in args {
        if let Some(flags) = a.strip_prefix('-') {
            if flags.is_empty() {
                continue;
            }
            if flags.chars().any(|c| matches!(c, 'w' | 'c' | 'd' | 'r')) {
                return false;
            }
            if flags.chars().any(|c| !matches!(c, 'l' | 'p')) {
                return false;
            }
        }
    }
    true
}

/// gpg 只读判定：仅 `--list-keys`/`--list-secret-keys` 列表操作；
/// `--export`/`--export-secret-keys`（私钥泄出）、`--import`/`--delete-*`
/// 等写操作拒绝。
/// gpg 写操作 flag：出现即拒绝（泄私钥/导入/删除/加解密/输出文件）。
const GPG_WRITE_FLAGS: &[&str] = &[
    "--export",
    "--export-secret-keys",
    "--export-secret-subkeys",
    "--import",
    "--delete-keys",
    "--delete-secret-keys",
    "--gen-key",
    "--quick-gen-key",
    "--full-gen-key",
    "--sign",
    "--clearsign",
    "--detach-sign",
    "--encrypt",
    "--decrypt",
    "--symmetric",
    "--armor",
    "--output",
    "--recipient",
    "--local-user",
    "--pinentry-mode",
    "--command-fd",
    "--passphrase",
];

/// gpg 只读判定：仅 `--list-keys`/`--list-secret-keys` 列表操作；
/// `--export`/`--export-secret-keys`（私钥泄出）、`--import`/`--delete-*`
/// 等写操作拒绝。
fn gpg_allowed(args: &[String]) -> bool {
    let has_list = args
        .iter()
        .any(|a| a == "--list-keys" || a == "--list-secret-keys" || a == "--with-colons");
    if !has_list {
        return false;
    }
    let has_write = args.iter().any(|a| GPG_WRITE_FLAGS.contains(&a.as_str()))
        || args.iter().any(|a| {
            GPG_WRITE_FLAGS
                .iter()
                .any(|w| a.starts_with(&format!("{w}=")))
        });
    !has_write
}

/// journalctl 只读判定：拒维护/清理操作（`--vacuum-*` 删日志、
/// `--rotate`/`--flush`/`--sync`），常规查询 flag 放行。
fn journalctl_allowed(args: &[String]) -> bool {
    !args.iter().any(|a| {
        a.starts_with("--vacuum")
            || matches!(
                a.as_str(),
                "--rotate"
                    | "--flush"
                    | "--sync"
                    | "--verify"
                    | "--setup-keys"
                    | "--update-catalog"
            )
    })
}

/// plutil 只读判定：仅 `-p`（print）/`-lint`（lint），精确 argv 匹配
/// （`-p` 前缀不可覆盖 `-convert` 等写形态）。
fn plutil_allowed(args: &[String]) -> bool {
    args.len() >= 2 && args.len() <= 3 && matches!(args[1].as_str(), "-p" | "-lint")
}

/// file 只读判定：位置参数与读取类 flag 均可，但 `-C`/`--compile`
/// 会把 `-m` 指定的 magic 文件编译写出 `magic.mgc`（写操作）。
/// 短 flag 粘连形态（`-Cv`）同样命中；`-c`（小写，打印 magic 检查）只读。
fn file_allowed(args: &[String]) -> bool {
    !args.iter().any(|a| {
        a == "-C"
            || a.starts_with("--compile")
            || (a.starts_with('-') && !a.starts_with("--") && a[1..].contains('C'))
    })
}

/// gh 只读判定：子命令前缀白名单 + 禁止写方法。
fn gh_allowed(args: &[String]) -> bool {
    // gh api 只允许 GET（`-X GET` / `--method GET` / `-XGET` / `--method=GET`）
    if args.get(1).map(|s| s.as_str()) == Some("api") {
        let rest = &args[2..];
        let mut i = 0;
        let mut has_payload = false;
        let mut explicit_get = false;
        while i < rest.len() {
            let a = &rest[i];
            let method = if let Some(m) = a.strip_prefix("--method") {
                // --method=GET 或 --method GET
                if let Some(v) = m.strip_prefix('=') {
                    Some(v.to_string())
                } else {
                    rest.get(i + 1).cloned()
                }
            } else if let Some(m) = a.strip_prefix("-X") {
                // -XGET 或 -X GET
                if !m.is_empty() {
                    Some(m.to_string())
                } else {
                    rest.get(i + 1).cloned()
                }
            } else {
                None
            };
            if let Some(m) = method {
                if !m.eq_ignore_ascii_case("GET") {
                    return false;
                }
                explicit_get = true;
            }
            // `-f`/`--field`（表单字段）、`-F`/`--raw-field`、`--input`
            // （请求体）存在时 gh 会把默认方法切换为 POST——创建/更新类
            // 写操作；仅显式 GET 时可安全视为只读。
            if is_gh_api_payload_flag(a) {
                has_payload = true;
            }
            i += 1;
        }
        return !has_payload || explicit_get;
    }
    let joined = args[1..].join(" ");
    if joined == "auth status" || joined.starts_with("auth status ") {
        // `--show-token` / `-t`（含 `--show-token=true` / `-t=true` 布尔形态）
        // 把 GitHub token 泄入输出（transcript）——拒绝；显式 `=false` 放行。
        return !args.iter().any(|a| is_token_display_flag(a));
    }
    GH_READONLY_SUBCOMMANDS
        .iter()
        .any(|s| joined.starts_with(s))
}

/// gh api 的写 payload flag：`-f`/`-F`（含 `-fKEY=VALUE` 粘连）、
/// `--field`/`--raw-field`/`--input`（含 `=` 形态）。
fn is_gh_api_payload_flag(a: &str) -> bool {
    if a == "-f" || a == "-F" || a == "--field" || a == "--raw-field" || a == "--input" {
        return true;
    }
    if a.starts_with("--field=") || a.starts_with("--raw-field=") || a.starts_with("--input=") {
        return true;
    }
    // pflag 短 flag 粘连：`-fname=value` / `-Fname=value`
    (a.starts_with("-f") || a.starts_with("-F")) && a.len() > 2
}

/// gh auth status 的 token 展示 flag 判定。
/// pflag 布尔 flag 接受 `--show-token=true` / `-t=true` 形态，同样会泄露
/// token；仅显式 pflag false 值（`0`/`f`/`F`/`FALSE`/`false`/`False`）时不展示。
fn is_token_display_flag(a: &str) -> bool {
    if let Some(v) = a.strip_prefix("--show-token=") {
        return !is_pflag_bool_false(v);
    }
    if let Some(v) = a.strip_prefix("-t=") {
        return !is_pflag_bool_false(v);
    }
    a == "--show-token" || a == "-t"
}

/// pflag 布尔 false 值判定（对齐 Go `strconv.ParseBool` 接受的 false 集合）。
///
/// pflag 的布尔 flag 解析底层调用 `strconv.ParseBool`，接受以下 false 值：
/// `0`/`f`/`F`/`FALSE`/`false`/`False`。其余值（含 `no`/`off`/空串）均被
/// pflag 拒绝并报错，命令不会执行——分类器对未知值保守返回"展示 token"
/// （即拒绝放行），安全姿态正确。
fn is_pflag_bool_false(v: &str) -> bool {
    matches!(v, "0" | "f" | "F" | "FALSE" | "false" | "False")
}

/// docker 只读判定：子命令前缀白名单。
fn docker_allowed(args: &[String]) -> bool {
    let joined = args[1..].join(" ");
    DOCKER_READONLY_SUBCOMMANDS
        .iter()
        .any(|s| joined.starts_with(s))
}

/// unzip 只读判定：必须出现 list 形态（短 flag 含 `l`），可带只读展示
/// flag（`-v`/`-z`/`-t`/`-Z`/`-q` 等）；解包/写形态字符（`-d` 输出目录、
/// `-o` 覆盖、`-j` 丢弃路径、`-n` 不覆盖、`-p` 管道提取、`-P` 口令）
/// 出现即拒绝。位置参数 = 归档文件/通配模式（只读）。
/// `unzip -l archive.zip` 放行；`unzip -l /tmp/x -d /etc` 拒绝（L5）。
fn unzip_list_allowed(args: &[String]) -> bool {
    let mut list_seen = false;
    for a in args {
        if a == "--" {
            continue;
        }
        let Some(body) = a.strip_prefix('-') else {
            continue; // 位置参数（归档文件/通配模式），只读
        };
        if body.is_empty() {
            continue;
        }
        for c in body.chars() {
            match c {
                'l' => list_seen = true,
                'd' | 'o' | 'j' | 'n' | 'p' | 'P' => return false,
                _ => {} // v/z/t/Z/q/s 等只读展示 flag
            }
        }
    }
    list_seen
}

/// 7z 只读判定：仅 list 动词（位置参数 `l`），后续 flag 只允许只读展示/
/// 过滤；`-o`（输出目录）与 `-w`（工作目录）是解包写形态，出现即拒绝。
/// 解包动词（`x`/`e`）是不同位置参数，不可能命中 `l`（L5）。
fn sevenz_list_allowed(args: &[String]) -> bool {
    if args.get(1).map(|s| s.as_str()) != Some("l") {
        return false;
    }
    args[2..]
        .iter()
        .all(|a| !a.starts_with("-o") && !a.starts_with("-w"))
}

// ---------------------------------------------------------------------------
// 入口
// ---------------------------------------------------------------------------

/// 四层分类命令是否只读（带命中形态追踪）。
///
/// 与 [`classify_readonly`] 同源：后者只取分类；本函数额外报告命中的
/// 分类形态，供 [`CommandAudit`]（exec 审计预览）展示"为什么这样分类"。
/// 语义与 `classify_readonly` 完全一致（同一判定过程，仅附带形态信息）。
fn classify_with_form(cmd: &str) -> (ReadOnlyKind, ReadonlyForm) {
    if cmd.trim().is_empty() {
        return (ReadOnlyKind::NotReadOnly, ReadonlyForm::Fallback);
    }
    // 危险路径形式（UNC/URL/SMB）与 token 引用状态无关，全局预检
    if has_dangerous_path_form(cmd) {
        return (ReadOnlyKind::Dangerous, ReadonlyForm::Dangerous);
    }

    let (args, closed, injected) = split_args(cmd);
    if !closed {
        // 引号未闭合：命令无法正常解析执行，保守拒绝
        return (ReadOnlyKind::NotReadOnly, ReadonlyForm::Fallback);
    }
    // 链式/重定向/命令替换不是危险注入，但会执行额外命令或写文件：
    // 归 NotReadOnly 走权限流程，且优先于只读表，防止
    // `ls $(rm -rf /)` 借任意参数安全表免询问放行。
    if injected {
        return (ReadOnlyKind::NotReadOnly, ReadonlyForm::Fallback);
    }
    let Some(first) = args.first() else {
        return (ReadOnlyKind::NotReadOnly, ReadonlyForm::Fallback);
    };

    // 第三层：精确形式
    for (bin, flag) in READONLY_EXACT {
        if args.len() == 2 && first == bin && args[1] == *flag {
            return (ReadOnlyKind::ReadOnly, ReadonlyForm::Exact);
        }
    }

    // 第四层：allowlist（git/gh/docker + 专项命令）
    let allowed = match first.as_str() {
        "git" => {
            // argv 级注入面：格式串注入与全局 `-c`/`--config-env` 配置注入
            // （core.pager 等可执行任意命令）→ 直接 Dangerous
            if git_format_injection(&args) || git_global_config_injection(&args) {
                return (ReadOnlyKind::Dangerous, ReadonlyForm::Dangerous);
            }
            git_allowed(&args)
        }
        "gh" => gh_allowed(&args),
        "docker" => docker_allowed(&args),
        "unzip" => unzip_list_allowed(&args[1..]),
        "7z" => sevenz_list_allowed(&args),
        "file" => file_allowed(&args[1..]),
        "printenv" => args.len() == 2 && !args[1].starts_with('-'),
        "find" => find_allowed(&args[1..]),
        "tar" => tar_allowed(&args[1..]),
        "openssl" => {
            // `openssl version` 已由 READONLY_COMMANDS 精确条目覆盖；
            // 此处只处理 `openssl x509` 形态
            if args.get(1).map(|s| s.as_str()) == Some("x509") {
                openssl_x509_allowed(&args[2..])
            } else {
                false
            }
        }
        "xattr" => xattr_allowed(&args[1..]),
        "gpg" => gpg_allowed(&args[1..]),
        "journalctl" => journalctl_allowed(&args[1..]),
        "plutil" => plutil_allowed(&args),
        "rg" => rg_allowed(&args),
        "yq" => yq_allowed(&args),
        _ => false,
    };
    if allowed {
        return (ReadOnlyKind::ReadOnly, ReadonlyForm::SubcommandAllowlist);
    }

    // 第一层：任意参数安全（含 "uname -a" 这类带固定参数的条目）。
    // 多词条目按 argv 边界匹配（`uname -a` 不匹配 `uname -all`），
    // 避免粘连 flag 误入只读表。
    // L5：多词条目默认**精确 argv 匹配**（完整参数序列相等）——尾部 flag
    // 可把只读形态转写成操作/泄密形态（`ssh-keygen -y -f /etc/shadow`、
    // `sysctl -a -w key=val`、`fc -l -s 10`）时不得免询问逃逸。仅
    // [`MULTIWORD_PREFIX_SAFE`] 中"写形态是独立子命令"的条目保持前缀匹配。
    let joined = args.join(" ");
    if READONLY_COMMANDS.iter().any(|c| {
        if c.contains(' ') {
            if MULTIWORD_PREFIX_SAFE.contains(c) {
                joined == *c || joined.starts_with(&format!("{c} "))
            } else {
                joined == *c
            }
        } else {
            first == *c
        }
    }) {
        return (ReadOnlyKind::ReadOnly, ReadonlyForm::Allowlist);
    }

    // 第二层：零参数安全
    if args.len() == 1 && READONLY_NOARGS.iter().any(|c| first == *c) {
        return (ReadOnlyKind::ReadOnly, ReadonlyForm::NoArgs);
    }

    (ReadOnlyKind::NotReadOnly, ReadonlyForm::Fallback)
}

/// 四层分类命令是否只读。
///
/// 返回 [`ReadOnlyKind::ReadOnly`] 表示命令被判定为只读（可免询问执行）；
/// [`ReadOnlyKind::NotReadOnly`] 表示存在写入可能，应走正常权限流程；
/// [`ReadOnlyKind::Dangerous`] 表示命中注入模式，应直接拒绝执行。
pub fn classify_readonly(cmd: &str) -> ReadOnlyKind {
    classify_with_form(cmd).0
}

/// git 全局区配置注入检测：子命令之前出现 `-c`/`--config-env`
/// （可注入 core.pager 等执行任意命令）。跳过 `--version`/`--help`/
/// `--no-pager`（不带参数）与 `-C`（带一个目录参数）。子命令之后的
/// `-c` 是合法只读 flag（`git log -c` combined diff），不在此列。
fn git_global_config_injection(args: &[String]) -> bool {
    let mut i = 1;
    loop {
        let Some(a) = args.get(i) else {
            return false;
        };
        if a == "--version" || a == "--help" || a == "--no-pager" {
            i += 1;
            continue;
        }
        if a == "-C" {
            i += 2;
            continue;
        }
        return a.starts_with("-c") || a.starts_with("--config-env");
    }
}

/// 便捷谓词：命令是否被判定为只读。
pub fn is_readonly_command(cmd: &str) -> bool {
    classify_readonly(cmd) == ReadOnlyKind::ReadOnly
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    // ── 第一层：任意参数安全 ──

    #[test]
    fn plain_readonly_commands() {
        assert!(is_readonly_command("ls -la"));
        assert!(is_readonly_command("cat /etc/hostname"));
        assert!(is_readonly_command("rg pattern src/"));
        assert!(is_readonly_command("find . -name '*.rs'"));
        assert!(is_readonly_command("du -sh ."));
        assert!(is_readonly_command("grep -r foo ."));
    }

    // ── 第二层：零参数 ──

    #[test]
    fn noargs_commands() {
        assert!(is_readonly_command("pwd"));
        assert!(is_readonly_command("whoami"));
        assert!(!is_readonly_command("pwd foo")); // 带参数不再是零参形式
    }

    // ── 第三层：精确形式 ──

    #[test]
    fn exact_commands() {
        assert!(is_readonly_command("node -v"));
        assert!(is_readonly_command("python3 --version"));
        assert!(!is_readonly_command("node"));
        assert!(!is_readonly_command("node -e 'process.exit()'"));
        // date/hostname 只放行精确只读形态，写形态必须走权限流程
        assert!(is_readonly_command("date -u"));
        assert!(is_readonly_command("date +%s"));
        assert!(is_readonly_command("hostname -f"));
        assert!(is_readonly_command("hostname -s"));
    }

    // ── 执行器/写形态 flag 不得免询问放行（rg --pre / yq -i）──

    #[test]
    fn rg_pre_is_executor_not_readonly() {
        // rg --pre 在每个匹配文件上执行外部命令（命令执行器）→ NotReadOnly
        assert_eq!(
            classify_readonly("rg --pre 'sh -c \"touch /tmp/pwn\"' pattern ."),
            ReadOnlyKind::NotReadOnly
        );
        assert_eq!(
            classify_readonly("rg --pre=cat pattern ."),
            ReadOnlyKind::NotReadOnly
        );
        // 常规搜索仍只读
        assert!(is_readonly_command("rg -l foo src/"));
        assert!(is_readonly_command("rg --type rust 'struct X' ."));
    }

    #[test]
    fn yq_inplace_is_write_not_readonly() {
        // yq -i/--inplace 就地写文件 → NotReadOnly（走权限流程）
        assert_eq!(
            classify_readonly("yq -i '.enabled = true' config.yml"),
            ReadOnlyKind::NotReadOnly
        );
        assert_eq!(
            classify_readonly("yq --inplace '.a.b' file.yaml"),
            ReadOnlyKind::NotReadOnly
        );
        // 读模式仍只读
        assert!(is_readonly_command("yq '.enabled' config.yml"));
    }

    // ── 链式/重定向/命令替换走权限流程 ──

    #[test]
    fn chain_redirect_substitution_goes_through_permission() {
        // 普通链式/重定向/命令替换不是注入向量：降级为 NotReadOnly，
        // 由权限门 Ask/allow 规则决定（不再在分类器层硬拒）。
        assert_eq!(
            classify_readonly("ls $(rm -rf /)"),
            ReadOnlyKind::NotReadOnly
        );
        assert_eq!(classify_readonly("echo ${HOME}"), ReadOnlyKind::NotReadOnly);
        assert_eq!(classify_readonly("ls; rm -rf /"), ReadOnlyKind::NotReadOnly);
        assert_eq!(
            classify_readonly("cat a && touch b"),
            ReadOnlyKind::NotReadOnly
        );
        assert_eq!(
            classify_readonly("cat a > /tmp/x"),
            ReadOnlyKind::NotReadOnly
        );
        assert_eq!(classify_readonly("ls `id`"), ReadOnlyKind::NotReadOnly);
    }

    // ── git allowlist ──

    #[test]
    fn git_readonly_subcommands() {
        assert!(is_readonly_command("git status"));
        assert!(is_readonly_command("git status --porcelain"));
        assert!(is_readonly_command("git diff"));
        assert!(is_readonly_command("git diff --cached"));
        assert!(is_readonly_command("git log --oneline -5"));
        assert!(is_readonly_command("git log --format='%h %s'"));
        assert!(is_readonly_command("git rev-parse --show-toplevel"));
        assert!(is_readonly_command("git ls-files"));
        assert!(is_readonly_command("git branch -a"));
        assert!(is_readonly_command("git tag -l"));
        assert!(is_readonly_command("git remote -v"));
        assert!(is_readonly_command("git config --get user.name"));
        assert!(is_readonly_command("git config --list"));
        assert!(is_readonly_command("git --version"));
        assert!(is_readonly_command("git show HEAD"));
    }

    #[test]
    fn git_write_operations_are_not_readonly() {
        assert!(!is_readonly_command("git add ."));
        assert!(!is_readonly_command("git commit -m x"));
        assert!(!is_readonly_command("git push"));
        assert!(!is_readonly_command("git branch -D foo"));
        assert!(!is_readonly_command("git tag -d v1"));
        assert!(!is_readonly_command("git remote add origin url"));
        assert!(!is_readonly_command("git config user.name x"));
        assert!(!is_readonly_command("git stash pop"));
        assert!(!is_readonly_command("git checkout main"));
        assert!(!is_readonly_command("git reset --hard"));
        assert!(!is_readonly_command("git rebase main"));
        assert!(!is_readonly_command("git cherry-pick abc"));
        assert!(!is_readonly_command("git clean -fd"));
    }

    #[test]
    fn git_dangerous_flags_rejected() {
        // `--output=` 写文件
        assert_eq!(
            classify_readonly("git diff --output=/tmp/x"),
            ReadOnlyKind::NotReadOnly
        );
        // 全局区 `-c` 配置注入（core.pager 等，可执行任意命令）→ Dangerous
        assert_eq!(
            classify_readonly("git -c core.pager='cat /etc/passwd' log"),
            ReadOnlyKind::Dangerous
        );
        // 格式串注入 `%G`/`%x`（Dangerous 层，直接拒绝）
        assert_eq!(
            classify_readonly("git log --format='%G?'"),
            ReadOnlyKind::Dangerous
        );
        assert_eq!(
            classify_readonly("git log --format='%x1b[31m'"),
            ReadOnlyKind::Dangerous
        );
        // `format:` 前缀是常规 pretty 语法，非注入（误报修复）
        assert!(is_readonly_command("git log --pretty='format:%s'"));
        // 未知 flag 保守拒绝
        assert!(!is_readonly_command("git log --exec=evil"));
    }

    #[test]
    fn git_global_flags_parse_correctly() {
        // --no-pager/--version 不带参数，只跳 1（修复误跳 2 的 bug）
        assert!(is_readonly_command("git --no-pager status"));
        assert!(is_readonly_command("git --no-pager diff"));
        // -C 带目录参数，跳 2
        assert!(is_readonly_command("git -C /tmp status"));
        // 子命令后的 -c 是合法只读 flag（combined diff）
        assert!(is_readonly_command("git log -c"));
    }

    #[test]
    fn git_unknown_subcommand_rejected() {
        assert!(!is_readonly_command("git unknown-command"));
        assert!(!is_readonly_command("git"));
    }

    // ── gh / docker ──

    #[test]
    fn gh_readonly_subcommands() {
        assert!(is_readonly_command("gh pr view 123"));
        assert!(is_readonly_command("gh repo view owner/repo"));
        assert!(is_readonly_command("gh api repos/owner/repo"));
        assert!(is_readonly_command("gh api -X GET /user"));
        // 带写 payload 必须显式 GET，否则 gh 隐式切 POST
        assert!(is_readonly_command("gh api --method GET -f x=y rate_limit"));
        assert!(is_readonly_command("gh api -XGET --field=x=y rate_limit"));
        assert!(!is_readonly_command("gh api -f x=y rate_limit"));
        assert!(!is_readonly_command("gh api --field=name=foo /user/repos"));
        assert!(!is_readonly_command("gh api -Fname=foo /user/repos"));
        assert!(!is_readonly_command("gh api --input body.json /repos"));
        assert!(!is_readonly_command("gh pr create"));
        assert!(!is_readonly_command("gh repo delete owner/repo"));
        assert!(!is_readonly_command("gh api -X POST /repos"));
    }

    #[test]
    fn file_compile_and_printenv_not_readonly() {
        // file：常规读取放行，`-C`/`--compile` 编译写 magic.mgc 必须走权限
        assert!(is_readonly_command("file foo"));
        assert!(is_readonly_command("file -f list.txt"));
        assert!(!is_readonly_command("file -C -m magic.txt"));
        assert!(!is_readonly_command("file -Cv magic.txt"));
        assert!(!is_readonly_command("file --compile -m magic.txt"));
        // printenv：只放行显式变量名，裸 printenv 会输出全部环境变量
        assert!(is_readonly_command("printenv HOME"));
        assert!(!is_readonly_command("printenv"));
        assert!(!is_readonly_command("printenv -0 HOME"));
    }

    #[test]
    fn multiword_entries_match_at_argv_boundary() {
        // 多词条目按 argv 边界匹配，粘连文本不再误入只读表
        assert!(is_readonly_command("uname -a"));
        assert!(is_readonly_command("timedatectl status"));
        assert!(!is_readonly_command("timedatectl statusx"));
        assert!(is_readonly_command("systemctl list-units --state=running"));
        assert!(!is_readonly_command("systemctl list-unitsx"));
    }

    // ── L5：多词白名单尾部 flag 转写逃逸（AUDIT-2026-08-08 L5）──

    #[test]
    fn multiword_readonly_entries_reject_trailing_write_flags() {
        // ssh-keygen -y 放行，-y -f（读任意文件泄入输出）拒绝
        assert!(is_readonly_command("ssh-keygen -y"));
        assert!(!is_readonly_command("ssh-keygen -y -f /etc/shadow"));
        assert!(!is_readonly_command("ssh-keygen -y -f ~/.ssh/id_rsa"));
        // ssh-keygen -l 放行，-l -p（改私钥口令 = 写）拒绝
        assert!(is_readonly_command("ssh-keygen -l"));
        assert!(!is_readonly_command("ssh-keygen -l -p -f ~/.ssh/id_rsa"));
        // sysctl -a 放行，-a -w（写内核参数）拒绝
        assert!(is_readonly_command("sysctl -a"));
        assert!(!is_readonly_command("sysctl -a -w net.ipv4.ip_forward=1"));
        // fc -l 放行，-s（重执行历史命令 = 执行）拒绝
        assert!(is_readonly_command("fc -l"));
        assert!(!is_readonly_command("fc -l -s 10"));
        // 精确多词条目：尾部位置参数不再免询问（走权限流程）
        assert!(!is_readonly_command("openssl version -a extra"));
        assert!(!is_readonly_command("timedatectl status set-time 12:00"));
        assert!(!is_readonly_command(
            "pkgutil --pkg-info com.foo.pkg --forget"
        ));
    }

    #[test]
    fn unzip_list_only_readonly() {
        // unzip -l 放行（含归档位置参数/只读展示 flag），解包形态拒绝
        assert!(is_readonly_command("unzip -l"));
        assert!(is_readonly_command("unzip -l archive.zip"));
        assert!(is_readonly_command("unzip -lv archive.zip"));
        assert!(is_readonly_command("unzip -l /tmp/x.zip '*.rs'"));
        assert!(!is_readonly_command("unzip -l /tmp/x -d /etc"));
        assert!(!is_readonly_command("unzip -l archive.zip -o"));
        assert!(!is_readonly_command("unzip -lo archive.zip"));
        assert!(!is_readonly_command("unzip archive.zip")); // 裸解包
        assert!(!is_readonly_command("unzip -d /etc archive.zip"));
    }

    #[test]
    fn sevenz_list_only_readonly() {
        // 7z l 放行，-o 输出目录（解包写）拒绝；解包动词 x 拒绝
        assert!(is_readonly_command("7z l archive.7z"));
        assert!(is_readonly_command("7z l -r archive.7z"));
        assert!(!is_readonly_command("7z l archive.7z -o/tmp"));
        assert!(!is_readonly_command("7z l archive.7z -w/tmp"));
        assert!(!is_readonly_command("7z x archive.7z"));
    }

    #[test]
    fn subcommand_style_prefix_entries_keep_useful_trailing_args() {
        // 写形态是独立子命令的条目仍允许尾部只读参数
        assert!(is_readonly_command("systemctl status sshd"));
        assert!(is_readonly_command("systemctl list-units --state=running"));
        assert!(is_readonly_command(
            "defaults read com.apple.finder AppleShowAllFiles"
        ));
        assert!(is_readonly_command(
            "log show --predicate 'process == \"sshd\"'"
        ));
        // 对应的写形态（不同子命令）不受前缀影响
        assert!(!is_readonly_command("systemctl start sshd"));
        assert!(!is_readonly_command(
            "defaults write com.apple.finder AppleShowAllFiles YES"
        ));
        assert!(!is_readonly_command("log erase"));
    }

    #[test]
    fn docker_readonly_subcommands() {
        assert!(is_readonly_command("docker ps"));
        assert!(is_readonly_command("docker images"));
        assert!(is_readonly_command("docker inspect foo"));
        assert!(is_readonly_command("docker logs app"));
        assert!(!is_readonly_command("docker run nginx"));
        assert!(!is_readonly_command("docker rm foo"));
        assert!(!is_readonly_command("docker build ."));
    }

    // ── tokenizer 边界 ──

    #[test]
    fn split_args_handles_quotes() {
        let (a, c, inj) = split_args("git log --format='%h %s'");
        assert_eq!(a, vec!["git", "log", "--format=%h %s"]);
        assert!(c);
        assert!(!inj);
        let (a, c, inj) = split_args("ls \"a b\" c");
        assert_eq!(a, vec!["ls", "a b", "c"]);
        assert!(c);
        assert!(!inj);
        let (a, c, inj) = split_args("echo a\\ b");
        assert_eq!(a, vec!["echo", "a b"]);
        assert!(c);
        assert!(!inj);
    }

    #[test]
    fn unclosed_quote_is_conservative() {
        // 未闭合引号：命令无法正常解析，分类为 NotReadOnly
        assert_eq!(
            classify_readonly("git log --format='%h"),
            ReadOnlyKind::NotReadOnly
        );
    }

    // ── 引用感知注入检测（修复原始串扫描误报）──

    #[test]
    fn quoted_metachars_are_literal() {
        // 引号内的 `|`/`;`/`>` 是字面量：合法只读命令不再误拒
        assert!(is_readonly_command("grep -E 'a|b' src/"));
        assert!(is_readonly_command("git log --format='%h;%s'"));
        assert!(is_readonly_command("ls 'a>b'"));
        assert!(is_readonly_command("cat 'x;y'"));
        assert!(is_readonly_command("wc -l 'a|b'"));
    }

    #[test]
    fn unquoted_metachars_go_through_permission() {
        // 未引用的链式/重定向/命令替换归 NotReadOnly，由权限流程裁决
        assert_eq!(classify_readonly("echo \"a\"|b"), ReadOnlyKind::NotReadOnly);
        assert_eq!(
            classify_readonly("git log | head -20"),
            ReadOnlyKind::NotReadOnly
        );
        assert_eq!(classify_readonly("ls & echo x"), ReadOnlyKind::NotReadOnly);
        // 未引用换行 = 新命令边界
        assert_eq!(classify_readonly("ls\nrm -rf /"), ReadOnlyKind::NotReadOnly);
    }

    // ── C2：/dev/tcp /dev/udp 伪设备硬拒 ──

    #[test]
    fn dev_tcp_pseudo_device_is_dangerous() {
        // `cat /dev/tcp/HOST/PORT` 是网络重定向（bash 伪设备），
        // 可读云元数据/本地服务并注入上下文——即使 cat 只读也不得放行。
        assert_eq!(
            classify_readonly("cat /dev/tcp/169.254.169.254/80"),
            ReadOnlyKind::Dangerous
        );
        assert_eq!(
            classify_readonly("cat /dev/udp/127.0.0.1/53"),
            ReadOnlyKind::Dangerous
        );
        // 变体：glob / 尾部斜杠形态
        assert_eq!(classify_readonly("cat /dev/tcp/*"), ReadOnlyKind::Dangerous);
        // 写形态（重定向）也必须硬拒——/dev/tcp 预检先于 injected 判定
        assert_eq!(
            classify_readonly("echo hi > /dev/tcp/x/80"),
            ReadOnlyKind::Dangerous
        );
        // 正例：普通路径 / 常规只读命令不受影响
        assert!(is_readonly_command("cat /etc/hostname"));
        assert!(is_readonly_command("ls /dev"));
        assert!(is_readonly_command("ls -la /etc/hosts"));
    }

    // ── C1：命令执行器不得进"任意参数安全"表 ──

    #[test]
    fn executors_are_not_readonly() {
        // env/find/xargs 是命令执行器：写形态必须走权限流程（NotReadOnly）
        assert!(!is_readonly_command("env rm -rf /"));
        assert!(!is_readonly_command("env FOO=1 rm -rf x"));
        assert!(!is_readonly_command("find . -exec rm {} +"));
        assert!(!is_readonly_command("find . -execdir rm {} \\;"));
        assert!(!is_readonly_command("find . -delete"));
        assert!(!is_readonly_command("find . -ok rm {} \\;"));
        assert!(!is_readonly_command("xargs rm file"));
        assert!(!is_readonly_command("xargs -0 sh -c 'rm -rf /'"));
        // 只读形态仍放行
        assert!(is_readonly_command("find . -name '*.rs'"));
        assert!(is_readonly_command("find src -type f -maxdepth 2"));
        assert!(is_readonly_command("find . -name x -print"));
    }

    // ── H2：前缀匹配收严（写 flag 拒绝）──

    #[test]
    fn tar_write_combination_rejected() {
        assert!(is_readonly_command("tar -tf x.tar"));
        assert!(is_readonly_command("tar -tvf x.tar"));
        assert!(!is_readonly_command("tar -txf x.tar"));
        assert!(!is_readonly_command("tar -czf out.tgz dir"));
        assert!(!is_readonly_command("tar -xf x.tar"));
    }

    #[test]
    fn openssl_write_forms_rejected() {
        assert!(is_readonly_command(
            "openssl x509 -in cert.pem -text -noout"
        ));
        assert!(!is_readonly_command("openssl x509 -req -signkey k -out c"));
        assert!(!is_readonly_command(
            "openssl x509 -req -new -key k -out r.csr"
        ));
    }

    #[test]
    fn xattr_write_forms_rejected() {
        assert!(is_readonly_command("xattr -l file"));
        assert!(is_readonly_command("xattr file"));
        assert!(!is_readonly_command("xattr -w attr val file"));
        assert!(!is_readonly_command("xattr -c file"));
        assert!(!is_readonly_command("xattr -d attr file"));
    }

    #[test]
    fn gpg_export_rejected() {
        assert!(is_readonly_command("gpg --list-keys"));
        assert!(is_readonly_command("gpg --list-secret-keys"));
        assert!(!is_readonly_command("gpg --list-keys --export-secret-keys"));
        assert!(!is_readonly_command("gpg --list-keys --export"));
    }

    #[test]
    fn journalctl_vacuum_rejected() {
        assert!(is_readonly_command("journalctl -b"));
        assert!(is_readonly_command("journalctl --since '1 hour ago'"));
        assert!(!is_readonly_command("journalctl --vacuum-time=1d"));
        assert!(!is_readonly_command("journalctl --vacuum-size=500M"));
        assert!(!is_readonly_command("journalctl --setup-keys"));
        assert!(!is_readonly_command("journalctl --update-catalog"));
    }

    #[test]
    fn gh_show_token_rejected() {
        assert!(is_readonly_command("gh auth status"));
        assert!(!is_readonly_command("gh auth status --show-token"));
    }

    #[test]
    fn plutil_exact_only() {
        assert!(is_readonly_command("plutil -p file.plist"));
        assert!(is_readonly_command("plutil -lint file.plist"));
        assert!(!is_readonly_command("plutil -convert xml1 file.plist"));
    }

    // ── R2：git submodule 执行器 / stash 写形态 / branch-tag 创建 ──

    #[test]
    fn git_submodule_foreach_is_executor() {
        // 与 find -exec 同型：在子模块内执行任意 shell 命令，不得判只读
        assert!(!is_readonly_command(
            "git submodule foreach 'touch /tmp/pwn'"
        ));
        assert!(!is_readonly_command("git submodule foreach 'rm -rf *'"));
        // update/deinit/add 是写操作
        assert!(!is_readonly_command("git submodule update"));
        assert!(!is_readonly_command("git submodule deinit foo"));
        assert!(!is_readonly_command(
            "git submodule add https://evil/repo.git"
        ));
        // status 是唯一只读子命令
        assert!(is_readonly_command("git submodule status"));
        assert!(is_readonly_command("git submodule status --recursive"));
    }

    #[test]
    fn git_stash_write_forms_rejected() {
        // 裸 stash = push（写操作）；flag 形态均为创建 stash
        assert!(!is_readonly_command("git stash"));
        assert!(!is_readonly_command("git stash -u"));
        assert!(!is_readonly_command("git stash -a"));
        assert!(!is_readonly_command("git stash -p"));
        assert!(!is_readonly_command("git stash -m msg"));
        // list/show 只读
        assert!(is_readonly_command("git stash list"));
        assert!(is_readonly_command("git stash show"));
        assert!(is_readonly_command("git stash show stash@{0}"));
    }

    #[test]
    fn git_branch_tag_creation_rejected() {
        // 位置参数 = 创建目标，无读 flag 时不得放行
        assert!(!is_readonly_command("git branch foo"));
        assert!(!is_readonly_command("git tag v1"));
        // 显式读 flag 放行
        assert!(is_readonly_command("git branch -a"));
        assert!(is_readonly_command("git branch --list"));
        assert!(is_readonly_command("git branch --show-current"));
        assert!(is_readonly_command("git tag -l"));
        assert!(is_readonly_command("git tag --list"));
    }

    #[test]
    fn gh_show_token_short_flag_rejected() {
        assert!(is_readonly_command("gh auth status"));
        assert!(!is_readonly_command("gh auth status --show-token"));
        assert!(!is_readonly_command("gh auth status -t"));
        // pflag 布尔 flag 的 `=true` 形态同样泄露 token
        assert!(!is_readonly_command("gh auth status --show-token=true"));
        assert!(!is_readonly_command("gh auth status -t=true"));
        // 显式 false 不展示 token
        assert!(is_readonly_command("gh auth status --show-token=false"));
        assert!(is_readonly_command("gh auth status -t=false"));
    }

    #[test]
    fn git_format_separate_value_injection() {
        // R2：`--format <value>` 分离值形态（M3 只覆盖 `=` 形态）
        assert_eq!(
            classify_readonly("git log --pretty '%G?'"),
            ReadOnlyKind::Dangerous
        );
        assert_eq!(
            classify_readonly("git log --format '%x1b[31m'"),
            ReadOnlyKind::Dangerous
        );
        // 合法分离值仍放行
        assert!(is_readonly_command("git log --pretty 'format:%s'"));
    }

    #[test]
    fn system_write_forms_not_readonly() {
        // R2：系统写形态不得进任意参数表
        assert!(!is_readonly_command("mount /dev/sdb1 /mnt"));
        assert!(!is_readonly_command("date -s '2020-01-01'"));
        assert!(!is_readonly_command("date -u -s '2020-01-01'"));
        assert!(!is_readonly_command("date +%s 01020304"));
        assert!(!is_readonly_command("hostname newname"));
        assert!(!is_readonly_command("hostname -f newname"));
        assert!(!is_readonly_command("hostname -s newname"));
        assert!(!is_readonly_command("timedatectl set-time 12:00"));
        assert!(!is_readonly_command("history -c"));
        // 只读形态仍放行
        assert!(is_readonly_command("date -u"));
        assert!(is_readonly_command("hostname -f"));
        assert!(is_readonly_command("timedatectl status"));
    }

    #[test]
    fn tar_exec_class_flags_rejected() {
        // R2：执行类长 flag 与分离写 flag（GNU tar 读路径可触发）
        assert!(!is_readonly_command(
            "tar -tf x.tar --checkpoint-action=exec=id"
        ));
        assert!(!is_readonly_command(
            "tar -t --checkpoint=1 --checkpoint-action=exec=sh"
        ));
        assert!(!is_readonly_command("tar -t -x f.tar"));
        assert!(!is_readonly_command("tar -t -c f.tar"));
        assert!(!is_readonly_command("tar -t -F script.sh f.tar"));
        // 纯 list 仍放行
        assert!(is_readonly_command("tar -tf x.tar"));
        assert!(is_readonly_command("tar -tvf x.tar"));
    }

    // ── exec 审计：CommandAudit / classify_with_form 命中形态 ──

    #[test]
    fn command_audit_tracks_matched_form_per_layer() {
        // 第一层：任意参数安全
        let a = CommandAudit::from_command("ls -la");
        assert_eq!(a.kind, ReadOnlyKind::ReadOnly);
        assert_eq!(a.form, ReadonlyForm::Allowlist);
        assert!(a.allow_without_prompt);
        // 第二层：零参数
        let a = CommandAudit::from_command("pwd");
        assert_eq!(a.kind, ReadOnlyKind::ReadOnly);
        assert_eq!(a.form, ReadonlyForm::NoArgs);
        // 第三层：精确形式
        let a = CommandAudit::from_command("node -v");
        assert_eq!(a.kind, ReadOnlyKind::ReadOnly);
        assert_eq!(a.form, ReadonlyForm::Exact);
        // 第四层：子命令 + flag 白名单
        let a = CommandAudit::from_command("git status --porcelain");
        assert_eq!(a.kind, ReadOnlyKind::ReadOnly);
        assert_eq!(a.form, ReadonlyForm::SubcommandAllowlist);
        let a = CommandAudit::from_command("tar -tf x.tar");
        assert_eq!(a.form, ReadonlyForm::SubcommandAllowlist);
        // 危险注入
        let a = CommandAudit::from_command("//evil/share");
        assert_eq!(a.kind, ReadOnlyKind::Dangerous);
        assert_eq!(a.form, ReadonlyForm::Dangerous);
        assert!(!a.allow_without_prompt);
        let a = CommandAudit::from_command("git -c core.pager='cat /etc/passwd' log");
        assert_eq!(a.kind, ReadOnlyKind::Dangerous);
        assert_eq!(a.form, ReadonlyForm::Dangerous);
        // 回退：非只读，走权限流程
        let a = CommandAudit::from_command("rm -rf /tmp/x");
        assert_eq!(a.kind, ReadOnlyKind::NotReadOnly);
        assert_eq!(a.form, ReadonlyForm::Fallback);
        assert!(!a.allow_without_prompt);
        // 链式组合归 NotReadOnly（Fallback），不误入只读表
        let a = CommandAudit::from_command("ls $(rm -rf /)");
        assert_eq!(a.kind, ReadOnlyKind::NotReadOnly);
        assert_eq!(a.form, ReadonlyForm::Fallback);
    }

    #[test]
    fn command_audit_explanation_is_non_empty_and_human_readable() {
        for cmd in [
            "ls -la",
            "pwd",
            "node -v",
            "git status",
            "rm -rf /",
            "//evil/share",
        ] {
            let a = CommandAudit::from_command(cmd);
            assert!(!a.explanation.is_empty(), "cmd={cmd}");
            assert!(
                !a.explanation.chars().any(char::is_control),
                "explanation 不得含控制字符: {:?}",
                a.explanation
            );
            assert!(
                a.explanation.len() >= 8,
                "explanation 过短: {:?}",
                a.explanation
            );
        }
    }

    #[test]
    fn command_audit_matches_classify_readonly() {
        // 一致性：预览分类与真实执行路径 `classify_readonly` 同源零漂移
        let samples = [
            "ls -la",
            "cat /etc/hostname",
            "pwd",
            "node -v",
            "date -u",
            "git status",
            "git log --oneline -5",
            "gh pr view 123",
            "docker ps",
            "tar -tf x.tar",
            "rm -rf /tmp/x",
            "git add .",
            "git -c core.pager='cat /etc/passwd' log",
            "git log --format='%G?'",
            "//evil/share",
            "cat /dev/tcp/169.254.169.254/80",
            "ls $(rm -rf /)",
            "echo ${HOME}",
            "",
            "grep -E 'a|b' src/",
        ];
        for cmd in samples {
            assert_eq!(
                CommandAudit::from_command(cmd).kind,
                classify_readonly(cmd),
                "CommandAudit 与 classify_readonly 分类必须一致: {cmd:?}"
            );
        }
    }

    #[test]
    fn readonly_kind_serde_roundtrip() {
        for kind in [
            ReadOnlyKind::ReadOnly,
            ReadOnlyKind::NotReadOnly,
            ReadOnlyKind::Dangerous,
        ] {
            let s = serde_json::to_string(&kind).unwrap();
            assert_eq!(serde_json::from_str::<ReadOnlyKind>(&s).unwrap(), kind);
        }
        assert_eq!(
            serde_json::from_str::<ReadOnlyKind>("\"read_only\"").unwrap(),
            ReadOnlyKind::ReadOnly
        );
    }
}
