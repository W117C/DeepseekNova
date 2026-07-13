use dpronix_tools::security::path::secure_resolve;
use std::path::Path;

#[test]
fn test_secure_resolve_scenarios() {
    let tmp = tempfile::tempdir().unwrap();
    let root = std::fs::canonicalize(tmp.path()).unwrap();

    // Case 4: Normal file inside workspace should be allowed
    let normal_path = Path::new("src/main.rs");
    let res = secure_resolve(&root, normal_path).unwrap();
    assert_eq!(res, root.join("src/main.rs"));

    // Case 1: Traversal escape should be blocked
    let bad_path = Path::new("../../etc/passwd");
    let res = secure_resolve(&root, bad_path);
    assert!(res.is_err(), "should block escape: {:?}", res);

    // Case 2: Traversal resolving outside should be blocked
    let bad_path_2 = Path::new("a/b/../../../../outside");
    let res = secure_resolve(&root, bad_path_2);
    assert!(res.is_err(), "should block escape: {:?}", res);

    // Case 3: Symlink pointing outside workspace should be blocked
    let link_path = root.join("bad_symlink");
    let target = Path::new("/private/tmp");
    #[cfg(unix)]
    {
        if std::os::unix::fs::symlink(target, &link_path).is_ok() {
            let res = secure_resolve(&root, Path::new("bad_symlink/some_file"));
            assert!(res.is_err(), "should block symlink escape: {:?}", res);
        }
    }
}
