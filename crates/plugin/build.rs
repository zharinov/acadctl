#[path = "bridge_protocol.rs"]
#[allow(dead_code, reason = "the build script only needs the packaged program")]
mod bridge_protocol;

use std::env;
use std::fs;
use std::path::PathBuf;

fn main() {
    cxx_build::bridge("src/lib.rs")
        .std("c++17")
        .compile("acadctl-plugin-cxxbridge");

    let source = bridge_protocol::execution_driver_source();
    let out_dir = PathBuf::from(env::var_os("OUT_DIR").expect("Cargo provides OUT_DIR"));
    fs::write(out_dir.join("execution-driver.lsp"), &source)
        .expect("write generated execution driver");

    if let Some(target_dir) = env::var_os("CARGO_TARGET_DIR") {
        let generated_dir = PathBuf::from(target_dir)
            .join("acadctl-generated")
            .join(env::var("PROFILE").expect("Cargo provides PROFILE"));
        fs::create_dir_all(&generated_dir).expect("create generated bundle input directory");
        fs::write(generated_dir.join("execution-driver.lsp"), source)
            .expect("write generated bundle execution driver");
    }

    println!("cargo:rerun-if-changed=src/lib.rs");
    println!("cargo:rerun-if-changed=bridge_protocol.rs");
    println!("cargo:rerun-if-changed=lisp/execution-driver.lsp");
    println!("cargo:rerun-if-changed=lisp/form-evaluator.lsp");
}
