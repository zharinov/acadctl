use std::ffi::c_char;

#[unsafe(no_mangle)]
pub extern "C" fn acadctl_hello_message() -> *const c_char {
    c"acadctl ready — Rust core reached".as_ptr()
}
