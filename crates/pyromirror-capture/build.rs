use std::env;

fn main() {
    let target_os = env::var("CARGO_CFG_TARGET_OS").unwrap_or_default();

    let mut build = cc::Build::new();
    build.cpp(true).std("c++17").include("include");

    match target_os.as_str() {
        "windows" => {
            build.file("src/capture_win.cpp");
            println!("cargo:rustc-link-lib=d3d11");
            println!("cargo:rustc-link-lib=dxgi");
        }
        "linux" => {
            // Also emits the link flags.
            let pipewire = pkg_config::Config::new()
                .atleast_version("0.3.40")
                .probe("libpipewire-0.3")
                .expect("libpipewire-0.3 development files are required (e.g. libpipewire-0.3-dev / pipewire-devel)");
            build.file("src/capture_linux.cpp");
            build.includes(&pipewire.include_paths);
            // PipeWire's C headers lean on constructs C++ compilers warn about.
            build.flag_if_supported("-Wno-missing-field-initializers");
            build.flag_if_supported("-Wno-unused-parameter");
        }
        other => panic!("pyromirror-capture has no backend for target OS `{}`", other),
    }

    build.compile("pyromirror_capture_native");
    println!("cargo:rerun-if-changed=build.rs");
    println!("cargo:rerun-if-changed=include/pyromirror_capture.h");
    println!("cargo:rerun-if-changed=src/capture_win.cpp");
    println!("cargo:rerun-if-changed=src/capture_linux.cpp");
}
