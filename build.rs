#[path = "scripts/brand_build.rs"]
mod brand_build;
fn main() {
    brand_build::configure(std::path::Path::new(env!("CARGO_MANIFEST_DIR")));
}
