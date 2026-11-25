fn main() {
    let set = protox::Compiler::new([".", "../../../../crates/ironflowx-proto/proto"])
        .unwrap()
        .include_imports(true)
        .open_file("service.proto")
        .unwrap()
        .encode_file_descriptor_set();
    let out = std::path::PathBuf::from(std::env::var_os("OUT_DIR").unwrap());
    if let Err(e) = ironflowx_codegen::generate(&set, &out) {
        println!("cargo::error={e}");
    }
}
