use std::env;
use std::path::PathBuf;

fn main() {
    let manifest_dir = PathBuf::from(env::var("CARGO_MANIFEST_DIR").unwrap());
    let pyrowave_src = manifest_dir.join("../../submodules/pyrowave");

    // Only build with CMake if the submodule is present
    if pyrowave_src.join("CMakeLists.txt").exists() {
        println!("cargo:rerun-if-changed={}", pyrowave_src.join("pyrowave.h").display());
        println!("cargo:rerun-if-changed={}", pyrowave_src.join("pyrowave_c.cpp").display());

        let dst = cmake::Config::new(&pyrowave_src)
            .define("CMAKE_BUILD_TYPE", "Release")
            .build_target("pyrowave-shared")
            .build();

        println!("cargo:rustc-link-search=native={}/build", dst.display());
        println!("cargo:rustc-link-search=native={}/lib", dst.display());
        println!("cargo:rustc-link-lib=pyrowave-shared");
    }
}
