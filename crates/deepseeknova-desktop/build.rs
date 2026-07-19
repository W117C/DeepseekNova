use std::path::Path;

fn main() {
    let frontend = Path::new("frontend/dist");

    if !frontend.exists() {
        panic!(
            "\n========================================================================\n\
             Frontend build missing!\n\n\
             Please compile the frontend before building the desktop application:\n\
             1. cd crates/deepseeknova-desktop/frontend\n\
             2. npm ci\n\
             3. npm run build\n\
             ========================================================================\n"
        );
    }

    // Tell Cargo to rerun this build script if frontend sources change,
    // avoiding node_modules and dist directories to prevent recursion loops.
    println!("cargo:rerun-if-changed=frontend/src");
    println!("cargo:rerun-if-changed=frontend/package.json");
    println!("cargo:rerun-if-changed=frontend/package-lock.json");

    tauri_build::build();
}
