fn main() {
    slint_build::compile("ui/app.slint").unwrap();
    println!("cargo:rerun-if-changed=ui/app.slint");
}
