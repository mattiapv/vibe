use std::{env, fs, path::PathBuf, process::Command};

fn run(cmd: &mut Command) {
    let status = cmd
        .status()
        .unwrap_or_else(|_| panic!("failed to run {cmd:?}"));
    assert!(status.success(), "command failed: {cmd:?}");
}

fn main() {
    let manifest_dir = PathBuf::from(env::var("CARGO_MANIFEST_DIR").unwrap());
    let out_dir = PathBuf::from(env::var("OUT_DIR").unwrap());
    let host = env::var("HOST").unwrap_or_default();
    let target = env::var("TARGET").unwrap_or_default();

    // Build the vmnet-helper. Logic extracted from /vendor/vmnet-helper/build.sh
    // Limit this to Apple hosts; on others, create a stub file so we can still run `cargo check` (e.g., from within Vibe =D)
    if host.contains("apple-darwin") && target.contains("apple-darwin") {
        let vmnet_dir = manifest_dir.join("vendor/vmnet-helper");
        let arch_dir = out_dir.join("vmnet-helper-build/arm64");
        let vmnet_helper_path = arch_dir.join("vmnet-helper");

        if !arch_dir.join("build.ninja").exists() {
            run(Command::new("meson")
                .arg("setup")
                .arg(&arch_dir)
                .arg("--cross-file")
                .arg("arm64.ini")
                .current_dir(&vmnet_dir));
        }
        run(Command::new("meson").args(["compile", "-C"]).arg(&arch_dir));

        run(Command::new("codesign")
            .args(["--force", "--verbose", "--entitlements"])
            .arg(vmnet_dir.join("entitlements.plist"))
            .args(["--sign", "-"])
            .arg(&vmnet_helper_path));

        println!(
            "cargo:rustc-env=BUNDLED_VMNET_HELPER_PATH={}",
            vmnet_helper_path.display()
        );

        for entry in fs::read_dir(&vmnet_dir).expect("read vendor dir").flatten() {
            let path = entry.path();
            if let Some("c" | "h" | "plist" | "ini") = path.extension().and_then(|e| e.to_str()) {
                println!("cargo:rerun-if-changed={}", path.display());
            }
        }
        println!(
            "cargo:rerun-if-changed={}",
            vmnet_dir.join("meson.build").display()
        );
    } else {
        let stub_path = out_dir.join("stub-vmnet-helper");
        fs::write(&stub_path, []).expect("write stub vmnet-helper");
        println!(
            "cargo:rustc-env=BUNDLED_VMNET_HELPER_PATH={}",
            stub_path.display()
        );
    }

    // Expose GIT_SHA and BUILD_DATE vars so Vibe can embed them in its version info
    {
        let sha = Command::new("git")
            .args(["rev-parse", "--short", "HEAD"])
            .output()
            .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_string())
            .unwrap_or_else(|_| "unknown".into());
        let build_date = Command::new("date")
            .args(["-u", "+%F"])
            .output()
            .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_string())
            .unwrap_or_else(|_| "unknown".into());

        println!("cargo:rustc-env=GIT_SHA={sha}");
        println!("cargo:rustc-env=BUILD_DATE={build_date}");
        println!("cargo:rerun-if-changed=.git/HEAD");
    }
}
