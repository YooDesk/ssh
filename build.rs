#[path = "scripts/brand_build.rs"]
mod brand_build;
fn main() {
    if std::env::var("CARGO_CFG_TARGET_OS").as_deref() == Ok("linux") {
        println!("cargo:rustc-cdylib-link-arg=-Wl,--exclude-libs,ALL");
    }

    let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR"));
    let logo = brand_build::configure(root);
    brand_build::configure_executable_icon(root, &logo);
}
