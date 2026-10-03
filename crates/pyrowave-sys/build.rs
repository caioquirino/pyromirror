use std::env;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

fn run(cmd: &mut Command) {
    let status = cmd
        .status()
        .unwrap_or_else(|e| panic!("failed to spawn {:?}: {}", cmd, e));
    assert!(status.success(), "command failed: {:?}", cmd);
}

/// PyroWave expects a pinned Granite checkout at `<pyrowave>/Granite`. Upstream does this with
/// `checkout_granite.sh`; replicate it here so the build also works without a POSIX shell.
fn ensure_granite(pyrowave_src: &Path) {
    let granite = pyrowave_src.join("Granite");
    let volk = granite.join("third_party/volk/CMakeLists.txt");
    let headers = granite.join("third_party/khronos/vulkan-headers/include");
    if volk.exists() && headers.exists() {
        return;
    }

    let script = fs::read_to_string(pyrowave_src.join("checkout_granite.sh"))
        .expect("submodules/pyrowave is empty; run `git submodule update --init`");
    let commit = script
        .lines()
        .find_map(|l| l.trim().strip_prefix("GRANITE_COMMIT="))
        .expect("GRANITE_COMMIT not found in checkout_granite.sh")
        .trim()
        .to_string();

    if !granite.join(".git").exists() {
        run(Command::new("git")
            .current_dir(pyrowave_src)
            .args(["clone", "https://github.com/Themaister/Granite", "Granite"]));
    }
    run(Command::new("git").current_dir(&granite).args(["fetch", "origin"]));
    run(Command::new("git").current_dir(&granite).args(["checkout", &commit]));
    for module in ["third_party/volk", "third_party/khronos/vulkan-headers"] {
        run(Command::new("git")
            .current_dir(&granite)
            .args(["submodule", "update", "--init", module]));
    }
}

fn has_ninja() -> bool {
    Command::new("ninja").arg("--version").output().is_ok()
}

fn main() {
    let manifest_dir = PathBuf::from(env::var("CARGO_MANIFEST_DIR").unwrap());
    let pyrowave_src = manifest_dir.join("../../submodules/pyrowave");
    let target_os = env::var("CARGO_CFG_TARGET_OS").unwrap();
    let target_env = env::var("CARGO_CFG_TARGET_ENV").unwrap_or_default();

    assert!(
        pyrowave_src.join("CMakeLists.txt").exists(),
        "submodules/pyrowave is missing; run `git submodule update --init`"
    );
    ensure_granite(&pyrowave_src);

    for f in ["pyrowave.h", "pyrowave_c.cpp", "CMakeLists.txt"] {
        println!("cargo:rerun-if-changed={}", pyrowave_src.join(f).display());
    }

    // The codec is always built optimised: a debug PyroWave is useless for realtime streaming.
    let mut cfg = cmake::Config::new(&pyrowave_src);
    cfg.profile("Release").build_target("pyrowave-shared");
    if has_ninja() {
        cfg.generator("Ninja");
    }
    let dst = cfg.build();
    let build_dir = dst.join("build");

    // Single-config generators (Ninja, Makefiles) put the library in build/, Visual Studio
    // generators in build/Release/.
    let lib_dirs = [build_dir.clone(), build_dir.join("Release")];
    for dir in &lib_dirs {
        println!("cargo:rustc-link-search=native={}", dir.display());
    }

    // On Windows the CMake target sets PREFIX "lib", so MSVC's import library is
    // libpyrowave-shared.lib. MinGW's ld resolves -lpyrowave-shared to libpyrowave-shared.dll.a.
    let (link_name, runtime_names): (&str, &[&str]) = match (target_os.as_str(), target_env.as_str()) {
        ("windows", "msvc") => ("libpyrowave-shared", &["libpyrowave-shared-0.dll"]),
        ("windows", _) => ("pyrowave-shared", &["libpyrowave-shared-0.dll"]),
        ("macos", _) => ("pyrowave-shared", &["libpyrowave-shared.0.dylib"]),
        _ => ("pyrowave-shared", &["libpyrowave-shared.so.0"]),
    };
    println!("cargo:rustc-link-lib=dylib={}", link_name);

    // Place the runtime library next to the final executables (target/<profile>/ and deps/ for
    // tests) so they run without PATH / LD_LIBRARY_PATH tweaks. OUT_DIR is
    // target/[<triple>/]<profile>/build/pyrowave-sys-<hash>/out.
    let out_dir = PathBuf::from(env::var("OUT_DIR").unwrap());
    let profile_dir = out_dir.ancestors().nth(3).unwrap().to_path_buf();
    for name in runtime_names {
        let src = lib_dirs
            .iter()
            .map(|d| d.join(name))
            .find(|p| p.exists())
            .unwrap_or_else(|| panic!("{} was not produced by the PyroWave build", name));
        for dest_dir in [profile_dir.clone(), profile_dir.join("deps")] {
            if dest_dir.exists() {
                fs::copy(&src, dest_dir.join(name))
                    .unwrap_or_else(|e| panic!("failed to copy {}: {}", name, e));
            }
        }
    }
}
