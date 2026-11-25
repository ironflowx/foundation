//! Compiles the proto when a wire feature is on, with no `protoc` and no `buf` (D51).

fn main() {
    #[cfg(feature = "__wire")]
    wire();
}

#[cfg(feature = "__wire")]
fn wire() {
    const FILES: [&str; 4] = [
        "ironflowx/v1/environment.proto",
        "ironflowx/v1/error.proto",
        "ironflowx/v1/identity.proto",
        "ironflowx/v1/key.proto",
    ];
    println!("cargo::rerun-if-changed=proto");
    let set = protox::Compiler::new(["proto"])
        .expect("the proto directory")
        .include_imports(true)
        .include_source_info(true)
        .open_files(FILES)
        .expect("the proto compiles")
        // Not `file_descriptor_set()`, which drops custom options.
        .encode_file_descriptor_set();
    let out = std::path::PathBuf::from(std::env::var_os("OUT_DIR").expect("cargo sets OUT_DIR"));
    let path = out.join("descriptors.bin");
    std::fs::write(&path, set).expect("OUT_DIR is writable");
    connectrpc_build::Config::new()
        .descriptor_set(path)
        .files(&FILES)
        .include_file("wire.rs")
        .gate_client_feature(true)
        .compile()
        .expect("the wire code generates");
}
