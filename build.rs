// Builds the native Remote Play shim (native/) with MSYS2 and links it.
use std::path::PathBuf;
use std::process::Command;

fn main() {
    let root = PathBuf::from(std::env::var("CARGO_MANIFEST_DIR").unwrap());
    let msys = std::env::var("NAGITO_MSYS").map(PathBuf::from).unwrap_or_else(|_| root.join(".toolchain/msys64"));
    let bash = msys.join("usr/bin/bash.exe");
    // OUT_DIR is target/<profile>/build/<pkg>/out; DLLs must sit next to the exe in target/<profile>.
    let out = PathBuf::from(std::env::var("OUT_DIR").unwrap());
    let exe_dir = out.ancestors().nth(3).unwrap().to_path_buf();
    let to_msys = |p: &std::path::Path| {
        let s = p.to_string_lossy().replace('\\', "/");
        format!("/{}{}", s[..1].to_lowercase(), &s[2..])
    };
    let status = Command::new(&bash)
        .args(["-lc", &format!("bash '{}' '{}'", to_msys(&root.join("native/build.sh")), to_msys(&exe_dir))])
        .status()
        .unwrap_or_else(|e| panic!("MSYS2 bash not found at {} ({e}). Set NAGITO_MSYS to your msys64 folder.", bash.display()));
    assert!(status.success(), "native Remote Play build failed");
    println!("cargo:rustc-link-search=native={}", root.join("native/build").display());
    for f in ["native/nagito_rp.c", "native/nagito_rp.h", "native/CMakeLists.txt", "native/build.sh"] {
        println!("cargo:rerun-if-changed={f}");
    }
}
