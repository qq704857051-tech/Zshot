fn main() {
    if std::env::var("CARGO_CFG_TARGET_OS").as_deref() != Ok("windows") {
        return;
    }
    let ico = std::path::PathBuf::from(std::env::var("CARGO_MANIFEST_DIR").unwrap())
        .join("../../assets/logo/logo.ico");
    println!("cargo:rerun-if-changed={}", ico.display());
    let mut res = winres::WindowsResource::new();
    res.set_icon(ico.to_str().expect("logo.ico path"));
    res.compile().expect("embed logo.ico");
}
