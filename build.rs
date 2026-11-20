fn main() {
    println!("cargo::rerun-if-changed=build.rs");
    println!("cargo::rustc-check-cfg=cfg(local_model)");
    let target = |key: &str| std::env::var(format!("CARGO_CFG_TARGET_{key}")).unwrap_or_default();
    if matches!(
        (
            target("OS").as_str(),
            target("ARCH").as_str(),
            target("ENV").as_str()
        ),
        ("macos", "aarch64", _) | ("linux", "x86_64" | "aarch64", "gnu")
    ) {
        println!("cargo::rustc-cfg=local_model");
    }
}
