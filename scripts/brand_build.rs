// WebService与独立原生引擎共享品牌构建输入；修改显示名不改变稳定产品身份。
pub fn configure(root: &std::path::Path) {
    use std::{env, fs, path::PathBuf};
    println!("cargo:rerun-if-env-changed=PRODUCT_BRAND_FILE");
    println!(
        "cargo:rerun-if-changed={}",
        root.join("scripts/brand_build.rs").display()
    );
    let file = env::var_os("PRODUCT_BRAND_FILE")
        .map(PathBuf::from)
        .unwrap_or_else(|| root.join("products/yoodesk.json"));
    assert!(file.is_absolute(), "PRODUCT_BRAND_FILE必须为绝对路径");
    println!("cargo:rerun-if-changed={}", file.display());
    let brand: serde_json::Value =
        serde_json::from_slice(&fs::read(&file).expect("读取外部品牌文件")).expect("解析品牌JSON");
    let object = brand.as_object().expect("品牌必须为对象");
    assert!(
        object.keys().all(|k| [
            "schema_version",
            "product_id",
            "display_name",
            "publisher",
            "version",
            "logo"
        ]
        .contains(&k.as_str())),
        "未知品牌字段"
    );
    assert_eq!(brand["schema_version"], 1, "品牌schema_version必须为1");
    let field = |key: &str| {
        let value = brand[key].as_str().expect("品牌字段必须为字符串");
        assert!(
            !value.trim().is_empty() && !value.chars().any(char::is_control),
            "品牌字段不能为空或包含控制字符"
        );
        value
    };
    let id = field("product_id");
    assert!(
        id.len() <= 32
            && id.as_bytes()[0].is_ascii_lowercase()
            && id
                .bytes()
                .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == b'-'),
        "无效product_id"
    );
    let name = field("display_name");
    assert!(name.chars().count() <= 80, "display_name过长");
    let _ = (field("publisher"), field("version"));
    let directory = file
        .parent()
        .expect("品牌目录")
        .canonicalize()
        .expect("品牌目录");
    let relative = std::path::Path::new(field("logo"));
    assert!(
        !relative.is_absolute()
            && relative
                .components()
                .all(|c| matches!(c, std::path::Component::Normal(_))),
        "logo必须是品牌目录内的相对路径"
    );
    let icon = directory
        .join(relative)
        .canonicalize()
        .expect("读取品牌图标");
    assert!(icon.starts_with(&directory), "品牌图标不能越出品牌目录");
    let mime = match icon.extension().and_then(|e| e.to_str()) {
        Some("svg") => "image/svg+xml",
        Some("png") => "image/png",
        Some("ico") => "image/x-icon",
        _ => panic!("品牌图标仅支持svg/png/ico"),
    };
    let bytes = fs::read(&icon).expect("读取图标");
    assert!(
        !bytes.is_empty() && bytes.len() <= 1024 * 1024,
        "图标必须介于1字节和1MiB"
    );
    println!("cargo:rerun-if-changed={}", icon.display());
    let output = PathBuf::from(env::var_os("OUT_DIR").expect("OUT_DIR")).join("product-icon");
    fs::write(&output, bytes).expect("写入品牌图标");
    println!("cargo:rustc-env=PRODUCT_ID={id}");
    println!("cargo:rustc-env=PRODUCT_DISPLAY_NAME={name}");
    println!("cargo:rustc-env=PRODUCT_ICON_PATH={}", output.display());
    println!("cargo:rustc-env=PRODUCT_ICON_MIME={mime}");
}
