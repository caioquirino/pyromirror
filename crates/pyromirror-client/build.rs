// Builds the helper for zero-copy display (Direct3D on Windows, DMA-BUFs on Linux), and on
// Windows gives the executable its icon and version details (shown in Explorer, on shortcuts and
// in the taskbar).
fn main() {
    println!("cargo:rerun-if-changed=build.rs");
    println!("cargo:rerun-if-changed=../pyromirror-gui/assets/pyromirror.ico");
    let target_os = std::env::var("CARGO_CFG_TARGET_OS").unwrap_or_default();
    if target_os == "linux" {
        // The planes the picture is decoded into (see the file). EGL and GBM are loaded at run
        // time, so there is nothing to link but the loader.
        println!("cargo:rerun-if-changed=src/dmabuf_planes.c");
        cc::Build::new().file("src/dmabuf_planes.c").compile("pyromirror_dmabuf_planes");
        println!("cargo:rustc-link-lib=dl");
    }
    if target_os != "windows" {
        return;
    }
    // The textures the picture is decoded into (see the file); plain C, nothing extra to link.
    println!("cargo:rerun-if-changed=src/d3d11_planes.c");
    cc::Build::new().file("src/d3d11_planes.c").warnings(false).compile("pyromirror_d3d11_planes");

    let mut resource = winresource::WindowsResource::new();
    resource
        .set_icon("../pyromirror-gui/assets/pyromirror.ico")
        .set("ProductName", "PyroMirror")
        .set("FileDescription", "PyroMirror remote desktop viewer")
        .set("CompanyName", "PyroMirror contributors")
        .set("LegalCopyright", "Apache License 2.0");
    resource.compile().expect("could not embed the Windows icon and version resource");
}
