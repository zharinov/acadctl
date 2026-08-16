fn main() {
    let protoc = protoc_bin_vendored::protoc_bin_path().expect("vendored protoc is unavailable");

    // Cargo runs build scripts single-threaded, before code generation reads this variable.
    // SAFETY: no other thread in this build-script process can read or mutate the environment.
    unsafe { std::env::set_var("PROTOC", protoc) };

    tonic_prost_build::configure()
        .bytes(".acadctl.ExecRequest.source")
        .compile_protos(&["proto/acadctl.proto"], &["proto"])
        .expect("failed to compile the RPC schema");
}
