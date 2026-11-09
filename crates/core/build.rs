fn main() {
    let mut config = prost_build::Config::new();
    config.protoc_executable(protoc_bin_vendored::protoc_bin_path().expect("bundled protoc"));
    config.type_attribute(".", "#[derive(serde::Serialize, serde::Deserialize)]");
    config
        .compile_protos(&["../../proto/analysis.proto"], &["../../proto"])
        .expect("analysis schema");
    println!("cargo:rerun-if-changed=../../proto/analysis.proto");
}
