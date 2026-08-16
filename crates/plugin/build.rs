#[path = "src/exec/protocol.rs"]
#[allow(dead_code, reason = "the build script only needs the packaged program")]
mod protocol;

use std::env;
use std::fmt::Write as _;
use std::fs;
use std::path::{Path, PathBuf};

fn main() {
    cxx_build::bridge("src/lib.rs")
        .std("c++17")
        .compile("acadctl-plugin-cxxbridge");

    let source = protocol::execution_driver_source();
    let out_dir = PathBuf::from(env::var_os("OUT_DIR").expect("Cargo provides OUT_DIR"));
    fs::write(out_dir.join("driver.lsp"), &source).expect("write generated execution driver");
    generate_printer_fixture_tests(&out_dir);

    if let Some(target_dir) = env::var_os("CARGO_TARGET_DIR") {
        let generated_dir = PathBuf::from(target_dir)
            .join("acadctl-generated")
            .join(env::var("PROFILE").expect("Cargo provides PROFILE"));
        fs::create_dir_all(&generated_dir).expect("create generated bundle input directory");
        fs::write(generated_dir.join("driver.lsp"), source)
            .expect("write generated bundle execution driver");
    }

    println!("cargo:rerun-if-changed=src/lib.rs");
    println!("cargo:rerun-if-changed=src/exec/protocol.rs");
    println!("cargo:rerun-if-changed=lisp/exec/driver.lsp");
    println!("cargo:rerun-if-changed=lisp/exec/evaluator.lsp");
}

fn generate_printer_fixture_tests(out_dir: &Path) {
    let fixture_dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/printer");
    let mut fixtures = fs::read_dir(&fixture_dir)
        .expect("read printer fixture directory")
        .map(|entry| entry.expect("read printer fixture entry").path())
        .filter(|path| path.extension().is_some_and(|extension| extension == "txt"))
        .collect::<Vec<_>>();
    fixtures.sort();

    let mut generated = String::new();

    for path in fixtures {
        let file_name = path
            .file_name()
            .and_then(|name| name.to_str())
            .expect("printer fixture name is UTF-8");
        let test_name = path
            .file_stem()
            .and_then(|name| name.to_str())
            .expect("printer fixture stem is UTF-8");
        let mut characters = test_name.chars();
        assert!(
            characters
                .next()
                .is_some_and(|character| character == '_' || character.is_ascii_alphabetic())
                && characters
                    .all(|character| character == '_' || character.is_ascii_alphanumeric()),
            "printer fixture name `{file_name}` is not a Rust test name"
        );
        writeln!(
            generated,
            "#[tokio::test]\nasync fn fixture_{test_name}() {{ run_fixture({file_name:?}, include_str!({path:?})).await; }}"
        )
        .expect("write generated printer test");
    }

    fs::write(out_dir.join("printer_fixtures.rs"), generated)
        .expect("write generated printer tests");
    println!("cargo:rerun-if-changed={}", fixture_dir.display());
}
