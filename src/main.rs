//! rsh — RustOS Shell
//!
//! Install the resulting ELF at `/bin/rsh` on your RustOS filesystem.
//! From the kernel shell:  `exec /bin/rsh`

#![no_std]
#![no_main]

#[macro_use]
pub mod io;
pub mod rt; // Minimal RustOS runtime (_start, #[panic_handler])
pub mod shell;
pub mod sys;

/// Called by `rt::_start`.  Returns the shell exit code.
#[no_mangle]
pub fn main() -> i64 {
    shell::run()
}
