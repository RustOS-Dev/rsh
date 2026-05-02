//! I/O utilities: `print!` / `println!` macros and raw byte helpers.
//!
//! All output goes through `rustos_rt::sys_write(1, ...)` (stdout).

use core::fmt;

// ── Writer ────────────────────────────────────────────────────────────────────

/// A zero-sized type that implements `core::fmt::Write` by forwarding to stdout.
pub struct StdoutWriter;

impl fmt::Write for StdoutWriter {
    #[inline]
    fn write_str(&mut self, s: &str) -> fmt::Result {
        crate::sys::sys_write(1, s.as_bytes());
        Ok(())
    }
}

/// Format `args` and write to stdout.
#[inline]
pub fn print_fmt(args: fmt::Arguments) {
    use fmt::Write;
    StdoutWriter.write_fmt(args).ok();
}

// ── Macros ────────────────────────────────────────────────────────────────────

/// Print formatted text to stdout (no newline).
#[macro_export]
macro_rules! print {
    ($($arg:tt)*) => {
        $crate::io::print_fmt(format_args!($($arg)*))
    };
}

/// Print formatted text to stdout followed by a newline.
#[macro_export]
macro_rules! println {
    () => { $crate::print!("\n") };
    ($($arg:tt)*) => { $crate::print!("{}\n", format_args!($($arg)*)) };
}

// ── Byte-level helpers ────────────────────────────────────────────────────────

/// Write a single byte to stdout.
#[inline]
pub fn write_byte(b: u8) {
    crate::sys::sys_write(1, core::slice::from_ref(&b));
}

/// Write a byte slice to stdout.
#[inline]
pub fn write_bytes(bs: &[u8]) {
    crate::sys::sys_write(1, bs);
}

/// Write a `&str` to stdout.
#[inline]
pub fn write_str(s: &str) {
    crate::sys::sys_write(1, s.as_bytes());
}

// ── Number formatting ─────────────────────────────────────────────────────────

/// Format a `u64` into `buf` as decimal.  Returns the used slice.
pub fn fmt_u64(mut n: u64, buf: &mut [u8; 20]) -> &[u8] {
    if n == 0 {
        buf[0] = b'0';
        return &buf[..1];
    }
    let mut pos = 20usize;
    while n > 0 {
        pos -= 1;
        buf[pos] = b'0' + (n % 10) as u8;
        n /= 10;
    }
    &buf[pos..]
}

/// Format an `i64` into `buf` as decimal.  Returns the used slice.
pub fn fmt_i64(n: i64, buf: &mut [u8; 21]) -> &[u8] {
    if n < 0 {
        buf[0] = b'-';
        let mut tmp = [0u8; 20];
        let s = fmt_u64(n.unsigned_abs(), &mut tmp);
        let len = s.len();
        buf[1..1 + len].copy_from_slice(s);
        &buf[..1 + len]
    } else {
        let mut tmp = [0u8; 20];
        let s = fmt_u64(n as u64, &mut tmp);
        let len = s.len();
        buf[..len].copy_from_slice(s);
        &buf[..len]
    }
}
