use std::env;

fn main() {
    let target_os = env::var("CARGO_CFG_TARGET_OS").unwrap_or_default();

    let mut build = cc::Build::new();
    build.cpp(true);
    build.std("c++17");
    build.include("include");

    if target_os == "windows" {
        build.file("src/capture_win.cpp");
        println!("cargo:rustc-link-lib=d3d11");
        println!("cargo:rustc-link-lib=dxgi");
    } else {
        build.file("src/capture_linux.cpp");
        if let Ok(lib) = pkg_config::probe_library("libpipewire-0.3") {
            for path in lib.include_paths {
                build.include(path);
            }
        }
        if let Ok(lib) = pkg_config::probe_library("dbus-1") {
            for path in lib.include_paths {
                build.include(path);
            }
        }
    }

    build.compile("pyromirror_capture_native");
    println!("cargo:rerun-if-changed=include/pyromirror_capture.h");
    println!("cargo:rerun-if-changed=src/capture_win.cpp");
    println!("cargo:rerun-if-changed=src/capture_linux.cpp");
}
