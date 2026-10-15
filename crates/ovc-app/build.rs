fn main() {
    #[cfg(windows)]
    {
        println!("cargo:rerun-if-changed=icon.ico");
        let mut res = winresource::WindowsResource::new();
        res.set_icon("icon.ico");
        res.set("ProductName", "MarkCam");
        res.set("FileDescription", "MarkCam — virtual camera & live video FX");
        res.set("CompanyName", "MarkCam");
        res.set("LegalCopyright", "Apache-2.0");
        if let Err(e) = res.compile() {
            // Don't fail the build if the resource compiler is unavailable.
            println!("cargo:warning=icon/resource embedding skipped: {e}");
        }
    }
}
