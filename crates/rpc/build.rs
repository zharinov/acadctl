fn main() {
    let protoc = protoc_bin_vendored::protoc_bin_path().expect("vendored protoc is unavailable");

    // Cargo runs build scripts single-threaded, before code generation reads this variable.
    unsafe { std::env::set_var("PROTOC", protoc) };

    tonic_prost_build::configure()
        .bytes(".acadctl.ExecutionRequest.source")
        .compile_protos(&["proto/acadctl.proto"], &["proto"])
        .expect("failed to compile the RPC schema");
}
