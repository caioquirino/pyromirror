fn main() {
    println!("cargo:rerun-if-changed=build.rs");
    println!("cargo:rerun-if-changed=src/capture_linux.cpp");

    // Windows uses cpal (WASAPI loopback) and needs no native code.
    if std::env::var("CARGO_CFG_TARGET_OS").as_deref() != Ok("linux") {
        return;
    }

    // Also emits the link flags.
    let pipewire = pkg_config::Config::new()
        .atleast_version("0.3.40")
        .probe("libpipewire-0.3")
        .expect("libpipewire-0.3 development files are required (e.g. libpipewire-0.3-dev / pipewire-devel)");

    cc::Build::new()
        .cpp(true)
        .std("c++17")
        .file("src/capture_linux.cpp")
        .includes(&pipewire.include_paths)
        .flag_if_supported("-Wno-missing-field-initializers")
        .flag_if_supported("-Wno-unused-parameter")
        .compile("pyromirror_audio_native");
}
