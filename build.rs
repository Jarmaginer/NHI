use std::process::Command;
use std::path::Path;

fn main() {
    // Tell Cargo to rerun this build script if daemon_wrapper.c changes
    println!("cargo:rerun-if-changed=src/daemon_wrapper.c");
    
    // Create target directory if it doesn't exist
    std::fs::create_dir_all("target").expect("Failed to create target directory");
    
    // Compile daemon_wrapper.c
    let output = Command::new("gcc")
        .args(&[
            "-Wall", 
            "-Wextra", 
            "-O2",
            "-o", "target/daemon_wrapper",
            "src/daemon_wrapper.c"
        ])
        .output()
        .expect("Failed to execute gcc command");

    if !output.status.success() {
        panic!(
            "Failed to compile daemon_wrapper.c:\n{}",
            String::from_utf8_lossy(&output.stderr)
        );
    }

    println!("Successfully compiled daemon_wrapper");
    
    // Make sure the binary is executable
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let path = Path::new("target/daemon_wrapper");
        if path.exists() {
            let mut perms = std::fs::metadata(path)
                .expect("Failed to get daemon_wrapper metadata")
                .permissions();
            perms.set_mode(0o755);
            std::fs::set_permissions(path, perms)
                .expect("Failed to set daemon_wrapper permissions");
        }
    }
}
