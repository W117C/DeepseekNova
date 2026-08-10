//! Property-based tests for the readonly command classifier.
//!
//! 验证 [`deepseeknova_security::readonly::classify_readonly`] 的安全不变量
//! 在任意输入下成立。对应 AGENTS.md §5 错误档案中多次分类器误判事故，
//! 用 property test 锁定"危险输入不得被判为 ReadOnly"的负向不变量。

use deepseeknova_security::readonly::{classify_readonly, ReadOnlyKind};
use proptest::prelude::*;

fn readonly_prefix() -> impl Strategy<Value = String> {
    prop::sample::select(vec![
        "ls".to_string(),
        "cat".to_string(),
        "grep".to_string(),
        "git status".to_string(),
        "git log --oneline".to_string(),
    ])
}

fn safe_token() -> impl Strategy<Value = String> {
    // 首字符不得为 `-` 或 `\`：避免生成的 token 在 gh api 测试中被
    // 分类器解析为 flag（如 `-XGET` 触发显式 GET 覆盖、`--method=POST`
    // 切写方法），导致 property test 出现 flaky 失败。
    "[A-Za-z0-9_./][A-Za-z0-9_./\\-]{0,19}"
}

/// 未引用时触发注入检测的 shell 链式/重定向字符。
fn shell_inject_char() -> impl Strategy<Value = char> {
    prop::sample::select(vec!['|', ';', '&', '>', '<'])
}

/// 命令替换/参数展开前缀（引号内外都执行）。
fn shell_subst_prefix() -> impl Strategy<Value = &'static str> {
    prop::sample::select(vec!["$(", "${", "`"])
}

/// pflag `ParseBool` 接受的 true 值集合（对齐 Go `strconv.ParseBool`）。
fn pflag_true_value() -> impl Strategy<Value = &'static str> {
    prop::sample::select(vec!["1", "t", "T", "TRUE", "true", "True"])
}

/// pflag `ParseBool` 接受的 false 值集合（对齐 Go `strconv.ParseBool`）。
fn pflag_false_value() -> impl Strategy<Value = &'static str> {
    prop::sample::select(vec!["0", "f", "F", "FALSE", "false", "False"])
}

/// tar 短 flag 组合中的写字符（解包/创建/追加/更新/脚本）。
fn tar_write_char() -> impl Strategy<Value = char> {
    prop::sample::select(vec!['x', 'c', 'r', 'u', 'F'])
}

/// tar 短 flag 组合中的只读字符（list + 展示 flag）。
fn tar_readonly_char() -> impl Strategy<Value = char> {
    prop::sample::select(vec!['t', 'v', 'z', 'p', 's'])
}

/// unzip 短 flag 组合中的写字符（输出目录/覆盖/丢弃路径/不覆盖/管道提取/口令）。
fn unzip_write_char() -> impl Strategy<Value = char> {
    prop::sample::select(vec!['d', 'o', 'j', 'n', 'p', 'P'])
}

/// unzip 短 flag 组合中的只读展示字符。
fn unzip_readonly_char() -> impl Strategy<Value = char> {
    prop::sample::select(vec!['l', 'v', 'z', 't', 'Z', 'q'])
}

/// xattr 短 flag 组合中的写字符（写/清/删/递归）。
fn xattr_write_char() -> impl Strategy<Value = char> {
    prop::sample::select(vec!['w', 'c', 'd', 'r'])
}

/// openssl x509 写 flag（签名请求/输出文件/签名密钥等）。
fn openssl_x509_write_flag() -> impl Strategy<Value = &'static str> {
    prop::sample::select(vec![
        "-req",
        "-out",
        "-signkey",
        "-new",
        "-newkey",
        "-set_serial",
        "-x509toreq",
        "-extfile",
        "-extensions",
        "-config",
        "-passin",
        "-passout",
    ])
}

proptest! {
    #[test]
    fn dev_tcp_always_dangerous(host in safe_token(), port in safe_token()) {
        let cmd = format!("cat /dev/tcp/{host}/{port}");
        prop_assert_eq!(
            classify_readonly(&cmd),
            ReadOnlyKind::Dangerous,
            "/dev/tcp/ 必须硬拒: {}", cmd
        );
    }

    #[test]
    fn dev_udp_always_dangerous(host in safe_token(), port in safe_token()) {
        let cmd = format!("cat /dev/udp/{host}/{port}");
        prop_assert_eq!(
            classify_readonly(&cmd),
            ReadOnlyKind::Dangerous,
            "/dev/udp/ 必须硬拒: {}", cmd
        );
    }

    #[test]
    fn dev_tcp_write_form_always_dangerous(host in safe_token(), port in safe_token()) {
        let cmd = format!("echo hi > /dev/tcp/{host}/{port}");
        prop_assert_eq!(
            classify_readonly(&cmd),
            ReadOnlyKind::Dangerous,
            "写形态 /dev/tcp/ 必须硬拒: {}", cmd
        );
    }

    #[test]
    fn unc_prefix_always_dangerous(suffix in safe_token()) {
        let cmd = format!("//{suffix}");
        prop_assert_eq!(
            classify_readonly(&cmd),
            ReadOnlyKind::Dangerous,
            "UNC // 前缀必须硬拒: {}", cmd
        );
    }

    #[test]
    fn unc_prefix_with_leading_whitespace_always_dangerous(
        ws in prop::collection::vec(prop::sample::select(vec![' ', '\t']), 0..4),
        suffix in safe_token()
    ) {
        let ws: String = ws.into_iter().collect();
        let cmd = format!("{ws}//{suffix}");
        prop_assert_eq!(
            classify_readonly(&cmd),
            ReadOnlyKind::Dangerous,
            "带前导空白的 UNC 必须硬拒: {:?}", cmd
        );
    }

    #[test]
    fn unquoted_pipe_never_readonly(
        prefix in readonly_prefix(),
        suffix in safe_token()
    ) {
        let cmd = format!("{prefix} | {suffix}");
        let kind = classify_readonly(&cmd);
        prop_assert_ne!(
            kind,
            ReadOnlyKind::ReadOnly,
            "未引用管道不得判只读: {}", cmd
        );
    }

    #[test]
    fn unquoted_semicolon_never_readonly(
        prefix in readonly_prefix(),
        suffix in safe_token()
    ) {
        let cmd = format!("{prefix}; {suffix}");
        let kind = classify_readonly(&cmd);
        prop_assert_ne!(kind, ReadOnlyKind::ReadOnly, "未引用分号不得判只读: {}", cmd);
    }

    #[test]
    fn unquoted_and_never_readonly(
        prefix in readonly_prefix(),
        suffix in safe_token()
    ) {
        let cmd = format!("{prefix} && {suffix}");
        let kind = classify_readonly(&cmd);
        prop_assert_ne!(kind, ReadOnlyKind::ReadOnly, "未引用 && 不得判只读: {}", cmd);
    }

    #[test]
    fn unquoted_redirect_never_readonly(
        prefix in readonly_prefix(),
        target in safe_token()
    ) {
        let cmd = format!("{prefix} > /tmp/{target}");
        let kind = classify_readonly(&cmd);
        prop_assert_ne!(kind, ReadOnlyKind::ReadOnly, "未引用 > 不得判只读: {}", cmd);
    }

    #[test]
    fn command_substitution_never_readonly(
        prefix in readonly_prefix(),
        inner in safe_token()
    ) {
        let cmd = format!("{prefix} $({inner})");
        let kind = classify_readonly(&cmd);
        prop_assert_ne!(
            kind,
            ReadOnlyKind::ReadOnly,
            "命令替换 $() 不得判只读: {}", cmd
        );
    }

    #[test]
    fn param_expansion_never_readonly(prefix in readonly_prefix(), var in safe_token()) {
        let cmd = format!("{prefix} ${{{var}}}");
        let kind = classify_readonly(&cmd);
        prop_assert_ne!(
            kind,
            ReadOnlyKind::ReadOnly,
            "参数展开 ${{}} 不得判只读: {}", cmd
        );
    }

    #[test]
    fn backtick_substitution_never_readonly(
        prefix in readonly_prefix(),
        inner in safe_token()
    ) {
        let cmd = format!("{prefix} `{inner}`");
        let kind = classify_readonly(&cmd);
        prop_assert_ne!(
            kind,
            ReadOnlyKind::ReadOnly,
            "反引号命令替换不得判只读: {}", cmd
        );
    }

    #[test]
    fn unquoted_newline_never_readonly(
        prefix in readonly_prefix(),
        suffix in safe_token()
    ) {
        let cmd = format!("{prefix}\n{suffix}");
        let kind = classify_readonly(&cmd);
        prop_assert_ne!(kind, ReadOnlyKind::ReadOnly, "未引用换行不得判只读: {:?}", cmd);
    }

    #[test]
    fn quoted_pipe_is_literal_stays_readonly(path in safe_token()) {
        let cmd = format!("grep -E 'a|b' {path}");
        prop_assert_eq!(
            classify_readonly(&cmd),
            ReadOnlyKind::ReadOnly,
            "引号内 | 是字面量，应判只读: {}", cmd
        );
    }

    #[test]
    fn git_global_config_injection_always_dangerous(
        key in safe_token(),
        value in safe_token(),
        subcommand in prop::sample::select(vec!["log", "status", "diff", "show"])
    ) {
        let cmd = format!("git -c {key}={value} {subcommand}");
        prop_assert_eq!(
            classify_readonly(&cmd),
            ReadOnlyKind::Dangerous,
            "git -c 全局配置注入必须硬拒: {}", cmd
        );
    }

    #[test]
    fn git_config_env_injection_always_dangerous(
        key in safe_token(),
        envvar in safe_token(),
        subcommand in prop::sample::select(vec!["log", "status", "diff", "show"])
    ) {
        let cmd = format!("git --config-env {key}={envvar} {subcommand}");
        prop_assert_eq!(
            classify_readonly(&cmd),
            ReadOnlyKind::Dangerous,
            "git --config-env 注入必须硬拒: {}", cmd
        );
    }

    #[test]
    fn git_format_percent_g_always_dangerous(suffix in safe_token()) {
        let cmd = format!("git log --format='%G{suffix}'");
        prop_assert_eq!(
            classify_readonly(&cmd),
            ReadOnlyKind::Dangerous,
            "git --format %G 注入必须硬拒: {}", cmd
        );
    }

    #[test]
    fn git_format_percent_x_always_dangerous(suffix in safe_token()) {
        let cmd = format!("git log --format='%x1b{suffix}'");
        prop_assert_eq!(
            classify_readonly(&cmd),
            ReadOnlyKind::Dangerous,
            "git --format %x 注入必须硬拒: {}", cmd
        );
    }

    #[test]
    fn git_pretty_separate_value_percent_g_always_dangerous(suffix in safe_token()) {
        let cmd = format!("git log --pretty '%G{suffix}'");
        prop_assert_eq!(
            classify_readonly(&cmd),
            ReadOnlyKind::Dangerous,
            "git --pretty 分离值 %G 注入必须硬拒: {}", cmd
        );
    }

    #[test]
    fn env_executor_never_readonly(arg in safe_token()) {
        let cmd = format!("env {arg}");
        let kind = classify_readonly(&cmd);
        prop_assert_ne!(kind, ReadOnlyKind::ReadOnly, "env 执行器不得判只读: {}", cmd);
    }

    #[test]
    fn find_exec_never_readonly(cmd_str in safe_token()) {
        let cmd = format!("find . -exec {cmd_str} \\;");
        let kind = classify_readonly(&cmd);
        prop_assert_ne!(
            kind,
            ReadOnlyKind::ReadOnly,
            "find -exec 执行器不得判只读: {}", cmd
        );
    }

    #[test]
    fn xargs_never_readonly(arg in safe_token()) {
        let cmd = format!("xargs {arg}");
        let kind = classify_readonly(&cmd);
        prop_assert_ne!(kind, ReadOnlyKind::ReadOnly, "xargs 执行器不得判只读: {}", cmd);
    }

    #[test]
    fn git_submodule_foreach_never_readonly(cmd_str in safe_token()) {
        let cmd = format!("git submodule foreach '{cmd_str}'");
        let kind = classify_readonly(&cmd);
        prop_assert_ne!(
            kind,
            ReadOnlyKind::ReadOnly,
            "git submodule foreach 执行器不得判只读: {}", cmd
        );
    }

    // ── date/hostname/timedatectl 写形态不变量 ──
    // 对应 error档案："多词'任意参数安全'前缀含写形态"事故：
    // date -u/hostname -f 等带固定参数条目按 starts_with 放行，导致
    // date -u -s ...、hostname -f newname 被免询问执行。

    #[test]
    fn date_set_with_u_flag_never_readonly(value in safe_token()) {
        let cmd = format!("date -u -s {value}");
        let kind = classify_readonly(&cmd);
        prop_assert_ne!(
            kind,
            ReadOnlyKind::ReadOnly,
            "date -u -s 写形态不得判只读: {}", cmd
        );
    }

    #[test]
    fn date_set_never_readonly(value in safe_token()) {
        let cmd = format!("date -s {value}");
        let kind = classify_readonly(&cmd);
        prop_assert_ne!(kind, ReadOnlyKind::ReadOnly, "date -s 写形态不得判只读: {}", cmd);
    }

    #[test]
    fn date_with_positional_arg_never_readonly(arg in safe_token()) {
        let cmd = format!("date +%s {arg}");
        let kind = classify_readonly(&cmd);
        prop_assert_ne!(
            kind,
            ReadOnlyKind::ReadOnly,
            "date 带位置参数不得判只读: {}", cmd
        );
    }

    #[test]
    fn hostname_set_never_readonly(newname in safe_token()) {
        let cmd = format!("hostname {newname}");
        let kind = classify_readonly(&cmd);
        prop_assert_ne!(
            kind,
            ReadOnlyKind::ReadOnly,
            "hostname 设置形态不得判只读: {}", cmd
        );
    }

    #[test]
    fn hostname_f_with_arg_never_readonly(newname in safe_token()) {
        let cmd = format!("hostname -f {newname}");
        let kind = classify_readonly(&cmd);
        prop_assert_ne!(
            kind,
            ReadOnlyKind::ReadOnly,
            "hostname -f 带参数不得判只读: {}", cmd
        );
    }

    #[test]
    fn timedatectl_set_time_never_readonly(value in safe_token()) {
        let cmd = format!("timedatectl set-time {value}");
        let kind = classify_readonly(&cmd);
        prop_assert_ne!(
            kind,
            ReadOnlyKind::ReadOnly,
            "timedatectl set-time 不得判只读: {}", cmd
        );
    }

    #[test]
    fn gh_api_implicit_post_with_f_flag_never_readonly(
        key in safe_token(),
        value in safe_token(),
        path in safe_token()
    ) {
        let cmd = format!("gh api -f {key}={value} {path}");
        let kind = classify_readonly(&cmd);
        prop_assert_ne!(
            kind,
            ReadOnlyKind::ReadOnly,
            "gh api -f 隐式 POST 不得判只读: {}", cmd
        );
    }

    #[test]
    fn gh_api_implicit_post_with_capital_f_never_readonly(
        key in safe_token(),
        value in safe_token(),
        path in safe_token()
    ) {
        let cmd = format!("gh api -F{key}={value} {path}");
        let kind = classify_readonly(&cmd);
        prop_assert_ne!(
            kind,
            ReadOnlyKind::ReadOnly,
            "gh api -F 隐式 POST 不得判只读: {}", cmd
        );
    }

    #[test]
    fn gh_api_implicit_post_with_input_never_readonly(
        file in safe_token(),
        path in safe_token()
    ) {
        let cmd = format!("gh api --input {file} {path}");
        let kind = classify_readonly(&cmd);
        prop_assert_ne!(
            kind,
            ReadOnlyKind::ReadOnly,
            "gh api --input 隐式 POST 不得判只读: {}", cmd
        );
    }

    #[test]
    fn gh_api_explicit_get_with_f_flag_stays_readonly(
        key in safe_token(),
        value in safe_token(),
        path in safe_token()
    ) {
        let cmd = format!("gh api --method GET -f {key}={value} {path}");
        prop_assert_eq!(
            classify_readonly(&cmd),
            ReadOnlyKind::ReadOnly,
            "显式 GET 应覆盖隐式 POST，判只读: {}", cmd
        );
    }

    // ── pflag 布尔 flag 完整值空间不变量（RR-1 修复）──
    // 对齐 Go strconv.ParseBool 接受的 true/false 值集合：
    // true:  1/t/T/TRUE/true/True  → 展示 token → NotReadOnly
    // false: 0/f/F/FALSE/false/False → 不展示 token → ReadOnly

    #[test]
    fn gh_auth_show_token_pflag_true_value_never_readonly(v in pflag_true_value()) {
        let cmd = format!("gh auth status --show-token={v}");
        prop_assert_ne!(
            classify_readonly(&cmd),
            ReadOnlyKind::ReadOnly,
            "pflag true 值 {} 展示 token，不得判只读: {}", v, cmd
        );
    }

    #[test]
    fn gh_auth_show_token_pflag_false_value_stays_readonly(v in pflag_false_value()) {
        let cmd = format!("gh auth status --show-token={v}");
        prop_assert_eq!(
            classify_readonly(&cmd),
            ReadOnlyKind::ReadOnly,
            "pflag false 值 {} 不展示 token，应判只读: {}", v, cmd
        );
    }

    #[test]
    fn gh_auth_short_t_pflag_true_value_never_readonly(v in pflag_true_value()) {
        let cmd = format!("gh auth status -t={v}");
        prop_assert_ne!(
            classify_readonly(&cmd),
            ReadOnlyKind::ReadOnly,
            "pflag true 值 {} 展示 token，不得判只读: {}", v, cmd
        );
    }

    #[test]
    fn gh_auth_short_t_pflag_false_value_stays_readonly(v in pflag_false_value()) {
        let cmd = format!("gh auth status -t={v}");
        prop_assert_eq!(
            classify_readonly(&cmd),
            ReadOnlyKind::ReadOnly,
            "pflag false 值 {} 不展示 token，应判只读: {}", v, cmd
        );
    }

    // ── tar 短 flag 组合写形态不变量 ──
    // 短 flag 组合中含 x/c/r/u/F（解包/创建/追加/更新/脚本）必须拒绝，
    // 即使同时含 t（list）也不得判只读。对应 error档案中"多词'任意参数安全'
    // 前缀含写形态"与"短 flag 组合误判"模式。

    #[test]
    fn tar_write_char_in_combination_never_readonly(
        write_c in tar_write_char(),
        extra in prop::collection::vec(tar_readonly_char(), 0..4),
        archive in safe_token()
    ) {
        let mut chars: String = extra.into_iter().collect();
        chars.push(write_c);
        let cmd = format!("tar -{} {}", chars, archive);
        prop_assert_ne!(
            classify_readonly(&cmd),
            ReadOnlyKind::ReadOnly,
            "tar 短 flag 含写字符 {} 不得判只读: {}", write_c, cmd
        );
    }

    #[test]
    fn tar_list_only_combination_stays_readonly(
        extra in prop::collection::vec(tar_readonly_char(), 0..4),
        archive in safe_token()
    ) {
        // 确保至少有一个 t（list）—— 全部用只读字符，但可能不含 t
        let mut chars: String = "t".to_string();
        chars.extend(extra);
        let cmd = format!("tar -{} {}", chars, archive);
        prop_assert_eq!(
            classify_readonly(&cmd),
            ReadOnlyKind::ReadOnly,
            "tar 纯 list 组合应判只读: {}", cmd
        );
    }

    // ── unzip 短 flag 组合写形态不变量 ──
    // 短 flag 组合中含 d/o/j/n/p/P（输出目录/覆盖/丢弃路径等）必须拒绝。

    #[test]
    fn unzip_write_char_in_combination_never_readonly(
        write_c in unzip_write_char(),
        extra in prop::collection::vec(unzip_readonly_char(), 0..4),
        archive in safe_token()
    ) {
        let mut chars: String = extra.into_iter().collect();
        chars.push(write_c);
        let cmd = format!("unzip -{} {}", chars, archive);
        prop_assert_ne!(
            classify_readonly(&cmd),
            ReadOnlyKind::ReadOnly,
            "unzip 短 flag 含写字符 {} 不得判只读: {}", write_c, cmd
        );
    }

    #[test]
    fn unzip_list_only_combination_stays_readonly(
        extra in prop::collection::vec(unzip_readonly_char(), 0..4),
        archive in safe_token()
    ) {
        let mut chars: String = "l".to_string();
        chars.extend(extra);
        let cmd = format!("unzip -{} {}", chars, archive);
        prop_assert_eq!(
            classify_readonly(&cmd),
            ReadOnlyKind::ReadOnly,
            "unzip 纯 list 组合应判只读: {}", cmd
        );
    }

    // ── 7z 输出目录写形态不变量 ──
    // 7z l（list）带 -o<dir>（输出目录）或 -w<dir>（工作目录）必须拒绝。

    #[test]
    fn sevenz_list_with_output_dir_never_readonly(dir in safe_token()) {
        let cmd = format!("7z l -o{dir} archive.7z");
        prop_assert_ne!(
            classify_readonly(&cmd),
            ReadOnlyKind::ReadOnly,
            "7z l -o 输出目录不得判只读: {}", cmd
        );
    }

    #[test]
    fn sevenz_list_with_work_dir_never_readonly(dir in safe_token()) {
        let cmd = format!("7z l -w{dir} archive.7z");
        prop_assert_ne!(
            classify_readonly(&cmd),
            ReadOnlyKind::ReadOnly,
            "7z l -w 工作目录不得判只读: {}", cmd
        );
    }

    #[test]
    fn sevenz_list_only_stays_readonly(archive in safe_token()) {
        let cmd = format!("7z l {archive}");
        prop_assert_eq!(
            classify_readonly(&cmd),
            ReadOnlyKind::ReadOnly,
            "7z 纯 list 应判只读: {}", cmd
        );
    }

    // ── xattr 短 flag 组合写形态不变量 ──
    // 短 flag 组合中含 w/c/d（写/清/删）必须拒绝。

    #[test]
    fn xattr_write_char_in_combination_never_readonly(
        write_c in xattr_write_char(),
        path in safe_token()
    ) {
        let cmd = format!("xattr -{write_c} {path}");
        prop_assert_ne!(
            classify_readonly(&cmd),
            ReadOnlyKind::ReadOnly,
            "xattr 短 flag 含写字符 {} 不得判只读: {}", write_c, cmd
        );
    }

    // ── openssl x509 写 flag 不变量 ──
    // openssl x509 带 -req/-out/-signkey 等写 flag 必须拒绝。

    #[test]
    fn openssl_x509_write_flag_never_readonly(
        flag in openssl_x509_write_flag(),
        cert in safe_token()
    ) {
        let cmd = format!("openssl x509 {flag} {cert}");
        prop_assert_ne!(
            classify_readonly(&cmd),
            ReadOnlyKind::ReadOnly,
            "openssl x509 带写 flag {} 不得判只读: {}", flag, cmd
        );
    }

    // ── RR-3 闭合：token 中嵌入 shell 元字符的不变量 ──
    // 对应残余风险 RR-3：property test 输入空间不生成 shell 元字符，
    // 链式/重定向/命令替换由结构化组合测试覆盖，但 token 内嵌入元字符
    // （如 `ls;rm`、`cat$(cmd)` 作为单个 token）的不变量未覆盖。
    // 以下测试在 safe_token 中间随机插入元字符，验证分类器对任意 token
    // 位置生成的元字符均能正确检测。

    #[test]
    fn token_with_embedded_inject_char_never_readonly(
        prefix in readonly_prefix(),
        inject_c in shell_inject_char(),
        head in safe_token(),
        tail in safe_token()
    ) {
        // 在 token 中间插入未引用链式/重定向字符：`prefix head<inject>tail`
        // split_args 的 cur_unquoted_inject 路径应命中 → NotReadOnly
        let cmd = format!("{prefix} {head}{inject_c}{tail}");
        prop_assert_ne!(
            classify_readonly(&cmd),
            ReadOnlyKind::ReadOnly,
            "token 含未引用元字符 {} 不得判只读: {}", inject_c, cmd
        );
    }

    #[test]
    fn token_with_embedded_subst_prefix_never_readonly(
        prefix in readonly_prefix(),
        subst in shell_subst_prefix(),
        head in safe_token(),
        tail in safe_token()
    ) {
        // 在 token 中间插入命令替换/参数展开前缀：`prefix head<subst>tail`
        // 命令替换引号内外都执行 → NotReadOnly
        let cmd = format!("{prefix} {head}{subst}{tail}");
        prop_assert_ne!(
            classify_readonly(&cmd),
            ReadOnlyKind::ReadOnly,
            "token 含命令替换前缀 {} 不得判只读: {}", subst, cmd
        );
    }

    #[test]
    fn token_with_newline_never_readonly(
        prefix in readonly_prefix(),
        head in safe_token(),
        tail in safe_token()
    ) {
        // 在 token 中间插入未引用换行：`prefix head\ntail`
        // split_args 的 injected = true 路径应命中 → NotReadOnly
        let cmd = format!("{prefix} {head}\n{tail}");
        prop_assert_ne!(
            classify_readonly(&cmd),
            ReadOnlyKind::ReadOnly,
            "token 含未引用换行不得判只读: {:?}", cmd
        );
    }

    #[test]
    fn quoted_metachar_in_argument_stays_readonly(
        path in safe_token(),
        inject_c in shell_inject_char()
    ) {
        // 反向不变量：引号内的元字符是字面量，不触发注入检测。
        // grep -E 'a|b' path 是只读命令，引号内 | 不构成管道。
        let cmd = format!("grep -E 'a{inject_c}b' {path}");
        prop_assert_eq!(
            classify_readonly(&cmd),
            ReadOnlyKind::ReadOnly,
            "引号内元字符 {} 是字面量，应判只读: {}", inject_c, cmd
        );
    }
}

// ── gh auth show-token 布尔 flag 不变量（无参数 property test，独立 #[test]）──
// 对应 error档案："布尔 flag 的 `=value` 形态绕过精确拒绝"事故：
// `gh auth status --show-token=true` 未被 `a == "--show-token"` 命中，
// token 泄露进 transcript。proptest! 宏要求至少一个 strategy 参数，
// 零参数断言以独立 #[test] 形式存在。

#[test]
fn gh_auth_show_token_eq_true_never_readonly() {
    let cmd = "gh auth status --show-token=true";
    assert_ne!(
        classify_readonly(cmd),
        ReadOnlyKind::ReadOnly,
        "--show-token=true 不得判只读"
    );
}

#[test]
fn gh_auth_short_t_eq_true_never_readonly() {
    let cmd = "gh auth status -t=true";
    assert_ne!(
        classify_readonly(cmd),
        ReadOnlyKind::ReadOnly,
        "-t=true 不得判只读"
    );
}

#[test]
fn gh_auth_show_token_no_value_never_readonly() {
    let cmd = "gh auth status --show-token";
    assert_ne!(
        classify_readonly(cmd),
        ReadOnlyKind::ReadOnly,
        "--show-token 不得判只读"
    );
}

#[test]
fn gh_auth_short_t_no_value_never_readonly() {
    let cmd = "gh auth status -t";
    assert_ne!(
        classify_readonly(cmd),
        ReadOnlyKind::ReadOnly,
        "-t 不得判只读"
    );
}

#[test]
fn gh_auth_show_token_eq_false_stays_readonly() {
    let cmd = "gh auth status --show-token=false";
    assert_eq!(
        classify_readonly(cmd),
        ReadOnlyKind::ReadOnly,
        "--show-token=false 不泄露 token，应判只读"
    );
}

#[test]
fn gh_auth_short_t_eq_false_stays_readonly() {
    let cmd = "gh auth status -t=false";
    assert_eq!(
        classify_readonly(cmd),
        ReadOnlyKind::ReadOnly,
        "-t=false 不泄露 token，应判只读"
    );
}

#[test]
fn quoted_semicolon_in_git_format_stays_readonly() {
    let cmd = "git log --format='%h;%s'";
    assert_eq!(
        classify_readonly(cmd),
        ReadOnlyKind::ReadOnly,
        "引号内 ; 是字面量，应判只读: {cmd}"
    );
}

#[test]
fn find_delete_never_readonly() {
    let cmd = "find . -delete";
    assert_ne!(
        classify_readonly(cmd),
        ReadOnlyKind::ReadOnly,
        "find -delete 不得判只读"
    );
}
