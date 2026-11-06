use std::{env, fs, path::PathBuf, process::Command};

fn main() {
    println!("cargo:rerun-if-changed=assets/transparentbar.ico");
    println!("cargo:rerun-if-changed=assets/app.rc");
    let target = env::var("TARGET").expect("Cargo TARGET");
    if !target.contains("windows") {
        return;
    }
    let output = PathBuf::from(env::var_os("OUT_DIR").expect("Cargo OUT_DIR"));
    if target.ends_with("msvc") {
        let resource = output.join("app.res");
        let compiler = env::var_os("RC").map(PathBuf::from).unwrap_or_else(|| {
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
        });
        let status = Command::new(compiler)
            .arg("/nologo")
            .arg("/fo")
            .arg(&resource)
            .current_dir("assets")
            .arg("app.rc")
            .status()
            .expect("run rc.exe");
        assert!(status.success(), "Windows icon resource compilation failed");
        println!(
            "cargo:rustc-link-arg-bin=transparentbar={}",
            resource.display()
        );
    } else {
        // ponytail: GNU local builds retain runtime icon; release CI embeds EXE resources using MSVC.
        println!("cargo:warning=Shell file icon resource embedded only in MSVC release builds");
    }
}
