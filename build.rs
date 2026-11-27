use std::{
    env, fs,
    path::{Path, PathBuf},
    process::Command,
};

fn latest_rc_in_kit_bin(bin_root: &Path) -> Option<PathBuf> {
    let mut versions: Vec<_> = fs::read_dir(bin_root)
        .ok()?
        .filter_map(Result::ok)
        .map(|entry| entry.path().join("x64/rc.exe"))
        .filter(|path| path.is_file())
        .collect();
    versions.sort();
    versions.pop()
}

fn find_rc_compiler() -> PathBuf {
    for var in ["RC", "RC_PATH"] {
        if let Some(path) = env::var_os(var).map(PathBuf::from) {
            if path.is_file() {
                return path;
            }
        }
    }
    if let Some(ver_bin) = env::var_os("WindowsSdkVerBinPath").map(PathBuf::from) {
        let candidate = ver_bin.join("x64/rc.exe");
        if candidate.is_file() {
            return candidate;
        }
    }
    if let Some(sdk_dir) = env::var_os("WindowsSdkDir").map(PathBuf::from) {
        if let Some(path) = latest_rc_in_kit_bin(&sdk_dir.join("bin")) {
            return path;
        }
    }
    if let Some(pf86) = env::var_os("ProgramFiles(x86)").map(PathBuf::from) {
        if let Some(path) = latest_rc_in_kit_bin(&pf86.join("Windows Kits/10/bin")) {
            return path;
        }
    }
    panic!("Windows SDK rc.exe not found (checked RC, RC_PATH, WindowsSdkVerBinPath, WindowsSdkDir, and Windows Kits/10/bin)")
}

fn compile_resource(compiler: &Path, rc_file: &str, out_res: &Path) {
    let status = Command::new(compiler)
        .arg("/nologo")
        .arg("/fo")
        .arg(out_res)
        .current_dir("assets")
        .arg(rc_file)
        .status()
        .expect("run rc.exe");
    assert!(
        status.success(),
        "Windows resource compilation failed for {rc_file}"
    );
}

fn main() {
    println!("cargo:rerun-if-changed=assets/transparentbar.ico");
    println!("cargo:rerun-if-changed=assets/app.rc");
    println!("cargo:rerun-if-changed=assets/app.manifest");
    println!("cargo:rerun-if-changed=assets/bridge.rc");
    println!("cargo:rerun-if-env-changed=RC");
    println!("cargo:rerun-if-env-changed=RC_PATH");
    println!("cargo:rerun-if-env-changed=WindowsSdkVerBinPath");
    println!("cargo:rerun-if-env-changed=WindowsSdkDir");
    let target = env::var("TARGET").expect("Cargo TARGET");
    if !target.contains("windows") {
        return;
    }
    let output = PathBuf::from(env::var_os("OUT_DIR").expect("Cargo OUT_DIR"));
    if target.ends_with("msvc") {
        println!("cargo:rustc-link-arg=/DEPENDENTLOADFLAG:0x800");
        println!("cargo:rustc-link-arg=/CETCOMPAT");

        let compiler = find_rc_compiler();
        let app_res = output.join("app.res");
        compile_resource(&compiler, "app.rc", &app_res);
        println!(
            "cargo:rustc-link-arg-bin=transparentbar={}",
            app_res.display()
        );
        // Manifest is embedded directly via app.rc (1 24 "app.manifest"), avoiding an external mt.exe PATH dependency.
        println!("cargo:rustc-link-arg-bin=transparentbar=/MANIFEST:NO");

        let bridge_res = output.join("bridge.res");
        compile_resource(&compiler, "bridge.rc", &bridge_res);
        println!("cargo:rustc-link-arg-cdylib={}", bridge_res.display());
    } else {
        // ponytail: GNU local builds retain runtime icon; release CI embeds EXE/DLL resources using MSVC.
        println!("cargo:warning=Shell file icon resource embedded only in MSVC release builds");
    }
}
