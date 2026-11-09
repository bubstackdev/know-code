fn main() {
    let directory = std::env::var("CARGO_MANIFEST_DIR").unwrap();
    std::fs::write(std::path::Path::new(&directory).join("know-code-build-ran"), "executed").unwrap();
    println!("cargo:rustc-cfg=know_code_generated");
}
