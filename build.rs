fn main() {
    slint_build::compile_with_config(
        "ui/app.slint",
        slint_build::CompilerConfiguration::new().with_bundled_translations("translations"),
    )
    .unwrap();
    println!("cargo:rerun-if-changed=ui/app.slint");
    println!("cargo:rerun-if-changed=translations");

    #[cfg(target_os = "windows")]
    {
        embed_resource::compile("REDR.rc", embed_resource::NONE)
            .manifest_optional()
            .unwrap();
    }
}
