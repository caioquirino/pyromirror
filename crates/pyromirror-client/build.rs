// Windows only: gives the executable its icon and version details (shown in Explorer, on
// shortcuts and in the taskbar), and builds the Direct3D helper for zero-copy display.
fn main() {
    println!("cargo:rerun-if-changed=build.rs");
    println!("cargo:rerun-if-changed=../pyromirror-gui/assets/pyromirror.ico");
    if std::env::var("CARGO_CFG_TARGET_OS").as_deref() != Ok("windows") {
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
