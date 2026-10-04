// Gives the Windows executable its icon and version details (shown in Explorer, on shortcuts
// and in the taskbar).
fn main() {
    println!("cargo:rerun-if-changed=build.rs");
    println!("cargo:rerun-if-changed=../pyromirror-gui/assets/pyromirror.ico");
    if std::env::var("CARGO_CFG_TARGET_OS").as_deref() != Ok("windows") {
        return;
    }
    let mut resource = winresource::WindowsResource::new();
    resource
        .set_icon("../pyromirror-gui/assets/pyromirror.ico")
        .set("ProductName", "PyroMirror")
        .set("FileDescription", "PyroMirror")
        .set("CompanyName", "PyroMirror contributors")
        .set("LegalCopyright", "Apache License 2.0");
    resource.compile().expect("could not embed the Windows icon and version resource");
}
