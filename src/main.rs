//! rsh — RustOS Shell
//!
//! Install the resulting ELF at `/bin/rsh` on your RustOS filesystem.
//! From the kernel shell:  `exec /bin/rsh`

#![no_std]
#![no_main]

extern crate rustos_rt;

#[macro_use]
pub mod io;
pub mod shell;
pub mod sys;

/// Called by `rustos_rt::_start`.  Returns the shell exit code.
#[no_mangle]
pub fn main() -> i64 {
    shell::run()
}
