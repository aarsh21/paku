fn main() {
    println!("cargo:rerun-if-changed=../../dist/windows/paku.rc");
    println!("cargo:rerun-if-changed=../../dist/windows/paku.ico");
    if std::env::var("CARGO_CFG_TARGET_OS").as_deref() == Ok("windows") {
        embed_resource::compile_for(
            "../../dist/windows/paku.rc",
            &["paku"],
            embed_resource::NONE,
        )
        .manifest_required()
        .expect("Windows app icon resource compilation failed");
    }
}
