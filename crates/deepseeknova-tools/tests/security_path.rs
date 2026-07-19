use deepseeknova_core::{Tool, ToolContext};
use deepseeknova_security::capability::Capability;
use deepseeknova_security::context::SecurityContext;
use deepseeknova_security::limits::ResourceLimits;
use deepseeknova_security::path::secure_resolve;
use deepseeknova_security::policy::SecurityPolicy;
use deepseeknova_tools::{
    GlobTool, GrepTool, LsTool, ReadFileTool, ShellTool, WebFetchTool, WriteFileTool,
};
use std::path::Path;

#[test]
fn test_secure_resolve_scenarios() {
    let tmp = tempfile::tempdir().unwrap();
    let root = std::fs::canonicalize(tmp.path()).unwrap();

    // Case 1: Normal file inside workspace should be allowed
    let normal_path = Path::new("src/main.rs");
    let res = secure_resolve(&root, normal_path).unwrap();
    assert_eq!(res, root.join("src/main.rs"));

    // Case 2: Traversal escape should be blocked
    let bad_path = Path::new("../../etc/passwd");
    let res = secure_resolve(&root, bad_path);
    assert!(res.is_err(), "should block escape: {:?}", res);

    // Case 3: Traversal resolving outside should be blocked
    let bad_path_2 = Path::new("a/b/../../../../outside");
    let res = secure_resolve(&root, bad_path_2);
    assert!(res.is_err(), "should block escape: {:?}", res);

    // Case 4: Absolute path escape should be blocked (Unix only — /etc is not absolute on Windows)
    #[cfg(unix)]
    {
        let abs_path = Path::new("/etc");
        let res = secure_resolve(&root, abs_path);
        assert!(res.is_err(), "should block absolute path escape: {:?}", res);
    }

    // Case 5: Symlink pointing outside workspace should be blocked
    #[cfg(unix)]
    {
        let outside_tmp = tempfile::tempdir().unwrap();
        let outside_path = std::fs::canonicalize(outside_tmp.path()).unwrap();
        let link_path = root.join("bad_symlink");

        if std::os::unix::fs::symlink(&outside_path, &link_path).is_ok() {
            let res = secure_resolve(&root, Path::new("bad_symlink/some_file"));
            assert!(res.is_err(), "should block symlink escape: {:?}", res);
        }
    }

    // Case 6: Broken symlink escape should be blocked
    let broken_link = root.join("broken_symlink");
    let broken_target = Path::new("/nonexistent_directory/file");
    #[cfg(unix)]
    {
        if std::os::unix::fs::symlink(broken_target, &broken_link).is_ok() {
            let res = secure_resolve(&root, Path::new("broken_symlink/file"));
            assert!(
                res.is_err(),
                "should block broken symlink escape: {:?}",
                res
            );
        }
    }
}

#[tokio::test]
async fn test_tool_security_boundaries_with_safe_defaults() {
    let tmp = tempfile::tempdir().unwrap();
    let root = std::fs::canonicalize(tmp.path()).unwrap();

    // Create a dummy file inside workspace
    let dummy_dir = root.join("src");
    std::fs::create_dir_all(&dummy_dir).unwrap();
    let dummy_file = dummy_dir.join("main.rs");
    std::fs::write(&dummy_file, "pub fn main() { println!(\"hello\"); }").unwrap();

    // Construct a ToolContext with safe defaults extension registered
    let ctx = ToolContext::new("call-1")
        .with_workspace(root.clone())
        .with_extension(SecurityContext::with_safe_defaults());

    // 1. Test LsTool
    let ls = LsTool;
    let res = ls.execute(&ctx, "{\"path\":\"src\"}").await;
    assert!(res.is_ok(), "ls src should succeed: {:?}", res);

    // 2. Test GlobTool
    let glob_tool = GlobTool;
    let res = glob_tool.execute(&ctx, "{\"pattern\":\"src/*.rs\"}").await;
    assert!(res.is_ok(), "glob inside should succeed: {:?}", res);

    // 3. Test GrepTool
    let grep_tool = GrepTool;
    let res = grep_tool
        .execute(&ctx, "{\"pattern\":\"hello\", \"path\":\"src\"}")
        .await;
    assert!(res.is_ok(), "grep inside should succeed: {:?}", res);
}

#[tokio::test]
async fn test_tool_missing_security_context_fails() {
    let tmp = tempfile::tempdir().unwrap();
    let root = std::fs::canonicalize(tmp.path()).unwrap();

    // Create a ToolContext WITHOUT any SecurityContext extension registered
    let ctx = ToolContext::new("call-2").with_workspace(root.clone());

    let ls = LsTool;
    let res = ls.execute(&ctx, "{\"path\":\"src\"}").await;
    assert!(
        res.is_err(),
        "ls tool must fail if SecurityContext is missing from ToolContext"
    );
}

#[tokio::test]
async fn test_tool_capability_escalation_denied() {
    let tmp = tempfile::tempdir().unwrap();
    let root = std::fs::canonicalize(tmp.path()).unwrap();

    let dummy_file = root.join("test.txt");
    std::fs::write(&dummy_file, "hello").unwrap();

    // Create a restricted context that ONLY has Capability::FileRead
    let mut caps = std::collections::HashSet::new();
    caps.insert(Capability::FileRead);
    let restricted_sec = SecurityContext {
        capabilities: caps,
        limits: ResourceLimits::default(),
        policy: SecurityPolicy::new(),
        audit: std::sync::Arc::new(deepseeknova_security::audit::TracingAuditLogger),
    };

    let ctx = ToolContext::new("call-3")
        .with_workspace(root.clone())
        .with_extension(restricted_sec);

    // ReadFileTool should succeed
    let read_tool = ReadFileTool;
    let res = read_tool.execute(&ctx, "{\"path\":\"test.txt\"}").await;
    assert!(
        res.is_ok(),
        "read file must succeed with FileRead capability"
    );

    // WriteFileTool should be denied
    let write_tool = WriteFileTool::new();
    let res = write_tool
        .execute(&ctx, "{\"path\":\"new.txt\", \"content\":\"hello\"}")
        .await;
    assert!(
        res.is_err(),
        "write file must fail (denied) without FileWrite capability"
    );
}

#[tokio::test]
async fn test_tool_resource_limits_enforced() {
    let tmp = tempfile::tempdir().unwrap();
    let root = std::fs::canonicalize(tmp.path()).unwrap();

    let dummy_dir = root.join("src");
    std::fs::create_dir_all(&dummy_dir).unwrap();
    let dummy_file_1 = dummy_dir.join("a.rs");
    let dummy_file_2 = dummy_dir.join("b.rs");
    std::fs::write(&dummy_file_1, "hello").unwrap();
    std::fs::write(&dummy_file_2, "hello").unwrap();

    // Create a context with strict limits: max_files = 1
    let mut caps = std::collections::HashSet::new();
    caps.insert(Capability::FileRead);
    let limits = ResourceLimits {
        max_files: 1,
        ..ResourceLimits::default()
    };

    let sec = SecurityContext {
        capabilities: caps,
        limits,
        policy: SecurityPolicy::new(),
        audit: std::sync::Arc::new(deepseeknova_security::audit::TracingAuditLogger),
    };

    let ctx = ToolContext::new("call-4")
        .with_workspace(root.clone())
        .with_extension(sec);

    let grep_tool = GrepTool;
    let res = grep_tool
        .execute(&ctx, "{\"pattern\":\"hello\", \"path\":\"src\"}")
        .await;
    assert!(res.is_ok());
    let output = res.unwrap();
    // It should have stopped search after 1 file limit
    assert!(
        output.contains("stopped after 1 files"),
        "output was: {}",
        output
    );
}

#[tokio::test]
async fn test_shell_command_blocking_policy() {
    let tmp = tempfile::tempdir().unwrap();
    let root = std::fs::canonicalize(tmp.path()).unwrap();

    // Setup policy to allow cargo but deny git / echo
    let mut caps = std::collections::HashSet::new();
    caps.insert(Capability::CommandExecute);
    let policy = SecurityPolicy {
        allowed_paths: Vec::new(),
        denied_paths: Vec::new(),
        allowed_commands: vec!["cargo".to_string()],
        allowed_domains: Vec::new(),
    };

    let sec = SecurityContext {
        capabilities: caps,
        limits: ResourceLimits::default(),
        policy,
        audit: std::sync::Arc::new(deepseeknova_security::audit::TracingAuditLogger),
    };

    let ctx = ToolContext::new("call-5")
        .with_workspace(root.clone())
        .with_extension(sec);

    let shell_tool = ShellTool::default();
    // cargo test should pass command check (starts with cargo)
    let res = shell_tool
        .execute(&ctx, "{\"command\":\"cargo --version\"}")
        .await;
    assert!(res.is_ok(), "cargo command should be allowed: {:?}", res);

    // git status should be blocked
    let res = shell_tool
        .execute(&ctx, "{\"command\":\"git status\"}")
        .await;
    assert!(res.is_err(), "git command should be blocked: {:?}", res);
}

#[tokio::test]
async fn test_network_domain_blocking_policy() {
    let tmp = tempfile::tempdir().unwrap();
    let root = std::fs::canonicalize(tmp.path()).unwrap();

    let mut caps = std::collections::HashSet::new();
    caps.insert(Capability::NetworkAccess);
    let policy = SecurityPolicy {
        allowed_paths: Vec::new(),
        denied_paths: Vec::new(),
        allowed_commands: Vec::new(),
        allowed_domains: vec!["example.com".to_string()],
    };

    let sec = SecurityContext {
        capabilities: caps,
        limits: ResourceLimits::default(),
        policy,
        audit: std::sync::Arc::new(deepseeknova_security::audit::TracingAuditLogger),
    };

    let ctx = ToolContext::new("call-6")
        .with_workspace(root.clone())
        .with_extension(sec);

    let web_tool = WebFetchTool;
    // example.com should pass
    let res = web_tool
        .execute(&ctx, "{\"url\":\"http://example.com/\"}")
        .await;
    if let Err(ref e) = res {
        assert!(
            !e.to_string().contains("domain 'example.com' is blocked"),
            "unexpected block error: {:?}",
            res
        );
    }

    // google.com should fail block check
    let res = web_tool
        .execute(&ctx, "{\"url\":\"http://google.com/\"}")
        .await;
    assert!(res.is_err());
    assert!(res
        .unwrap_err()
        .to_string()
        .contains("blocked by security policy"));
}
