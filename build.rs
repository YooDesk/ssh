#[path = "scripts/brand_build.rs"]
mod brand_build;
fn main() {
    if std::env::var("CARGO_CFG_TARGET_OS").as_deref() == Ok("linux") {
        println!("cargo:rustc-cdylib-link-arg=-Wl,--exclude-libs,ALL");
    }

    // 本仓库只产出 cdylib 插件，没有 bin target；EXE 图标属于宿主程序。
    let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR"));
    let _ = brand_build::configure(root);
}
