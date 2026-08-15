fn main() {
    println!("cargo:rerun-if-changed=windows/bootty.rc");
    println!("cargo:rerun-if-changed=assets/bootty.ico");

    if std::env::var("CARGO_CFG_TARGET_OS").unwrap_or_default() == "windows" {
        embed_resource::compile("windows/bootty.rc", embed_resource::NONE)
            .manifest_optional()
            .expect("compile Windows resources");
    }
}
