use std::{
    env, fs,
    path::{Path, PathBuf},
    process::Command,
};

fn find_rc_compiler() -> PathBuf {
    env::var_os("RC").map(PathBuf::from).unwrap_or_else(|| {
        let sdk =
            PathBuf::from(env::var_os("ProgramFiles(x86)").expect("Windows SDK ProgramFiles"))
                .join("Windows Kits/10/bin");
        let mut versions: Vec<_> = fs::read_dir(sdk)
            .expect("Windows SDK installed")
            .filter_map(Result::ok)
            .map(|entry| entry.path().join("x64/rc.exe"))
            .filter(|path| path.is_file())
            .collect();
        versions.sort();
        versions.pop().expect("Windows SDK rc.exe")
    })
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
    let target = env::var("TARGET").expect("Cargo TARGET");
    if !target.contains("windows") {
        return;
    }
    let output = PathBuf::from(env::var_os("OUT_DIR").expect("Cargo OUT_DIR"));
    if target.ends_with("msvc") {
        let compiler = find_rc_compiler();
        let app_res = output.join("app.res");
        compile_resource(&compiler, "app.rc", &app_res);
        println!(
            "cargo:rustc-link-arg-bin=transparentbar={}",
            app_res.display()
        );
        let manifest_dir =
            PathBuf::from(env::var_os("CARGO_MANIFEST_DIR").expect("CARGO_MANIFEST_DIR"));
        let manifest = manifest_dir.join("assets/app.manifest");
        println!("cargo:rustc-link-arg-bin=transparentbar=/MANIFEST:EMBED");
        println!(
            "cargo:rustc-link-arg-bin=transparentbar=/MANIFESTINPUT:{}",
            manifest.display()
        );

        let bridge_res = output.join("bridge.res");
        compile_resource(&compiler, "bridge.rc", &bridge_res);
        println!("cargo:rustc-link-arg-cdylib={}", bridge_res.display());
    } else {
        // ponytail: GNU local builds retain runtime icon; release CI embeds EXE/DLL resources using MSVC.
        println!("cargo:warning=Shell file icon resource embedded only in MSVC release builds");
    }
}
