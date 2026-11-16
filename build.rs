fn main() {
    println!("cargo:rerun-if-changed=assets/icon.ico");
    embed_windows_icon();
}

/// Embeds `assets/icon.ico` so Explorer, the Start menu and pinned shortcuts
/// use the same icon as the running window.
///
/// This runs only when the build host is Windows. Cross-compiling from another
/// platform leaves the resource out; the runtime window icon still works.
#[cfg(windows)]
fn embed_windows_icon() {
    let mut resource = winresource::WindowsResource::new();
    resource.set_icon("assets/icon.ico");
    resource.set("ProductName", "Terraria Content Extractor");
    resource.set("FileDescription", "Terraria Content Extractor");
    resource.set("InternalName", "terraria-content-extractor");
    resource.set("OriginalFilename", "terraria-content-extractor.exe");
    resource.set("LegalCopyright", "MIT License; icon artwork by Re-Logic");
    if let Err(error) = resource.compile() {
        println!("cargo:warning=could not embed assets/icon.ico: {error}");
    }
}

#[cfg(not(windows))]
fn embed_windows_icon() {}
