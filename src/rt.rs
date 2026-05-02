//! Minimal RustOS userspace runtime — inlined from `rustos-rt`.
//!
//! Provides:
//! * `_start`          — ELF entry point called by the RustOS process loader.
//! * `#[panic_handler]` — calls `sys_exit(1)` on panic.
//!
//! The high-level syscall wrappers (`sys_write`, `sys_read`, `sys_exit`, …)
//! live in `crate::sys` so they can be used from the rest of the crate too.

use core::panic::PanicInfo;

// The user entry point — defined in main.rs.
extern "Rust" {
    fn main() -> i64;
}

/// ELF entry point.  The RustOS kernel jumps here after mapping the binary.
///
/// # Safety
/// Must only be called once, by the kernel's ELF loader, with a valid stack.
#[no_mangle]
pub unsafe extern "C" fn _start() -> ! {
    // Zero the BSS segment so that all `static mut` variables start at zero.
    extern "C" {
        static mut __bss_start: u8;
        static mut __bss_end: u8;
    }
    let bss_len =
        (&raw const __bss_end as usize) - (&raw const __bss_start as usize);
    core::ptr::write_bytes(&raw mut __bss_start, 0, bss_len);

    let exit_code = main();
    crate::sys::sys_exit(exit_code);
}

/// Minimal panic handler: exit with code 1.
#[panic_handler]
fn panic(_info: &PanicInfo) -> ! {
    crate::sys::sys_exit(1)
}
