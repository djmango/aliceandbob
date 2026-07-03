fn main() -> Result<(), Box<dyn std::error::Error>> {
    connectrpc_axum_build::compile_dir("../proto")
        .include_file("protos.rs")
        .fetch_protoc(None, None)?
        .compile()?;
    println!("cargo:rerun-if-changed=../proto");
    Ok(())
}
