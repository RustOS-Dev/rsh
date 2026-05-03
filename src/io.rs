//! I/O utilities: `print!` / `println!` macros and raw byte helpers.
//!
//! All output goes through `rustos_rt::sys_write(1, ...)` (stdout), unless
//! pipe-capture mode is active, in which case output is redirected into an
//! in-process buffer so it can be piped to the next command.

use core::fmt;

// ── Pipe capture ──────────────────────────────────────────────────────────────

/// Size of the in-process pipe capture buffer (bytes).
pub const PIPE_BUF_SIZE: usize = 4096;

static mut PIPE_CAPTURE_BUF: [u8; PIPE_BUF_SIZE] = [0u8; PIPE_BUF_SIZE];
static mut PIPE_CAPTURE_LEN: usize = 0;
static mut PIPE_CAPTURING: bool = false;

/// Start capturing all subsequent output into the internal pipe buffer.
/// Resets the buffer length to zero.
pub fn pipe_capture_start() {
    unsafe {
        PIPE_CAPTURING = true;
        PIPE_CAPTURE_LEN = 0;
    }
}

/// Stop capturing and return the bytes that were captured.
/// The returned slice is valid until the next call to `pipe_capture_start`.
pub fn pipe_capture_end() -> &'static [u8] {
    unsafe {
        PIPE_CAPTURING = false;
        &PIPE_CAPTURE_BUF[..PIPE_CAPTURE_LEN]
    }
}

/// Returns `true` while output is being captured for a pipe.
pub fn is_capturing() -> bool {
    unsafe { PIPE_CAPTURING }
}

// ── Writer ────────────────────────────────────────────────────────────────────

/// A zero-sized type that implements `core::fmt::Write` by forwarding to
/// `write_bytes`, which respects the current pipe-capture mode.
pub struct StdoutWriter;

impl fmt::Write for StdoutWriter {
    #[inline]
    fn write_str(&mut self, s: &str) -> fmt::Result {
        write_bytes(s.as_bytes());
        Ok(())
    }
}

/// Format `args` and write to stdout (or the active pipe buffer).
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

/// Write `bs` to stdout, or into the pipe capture buffer if capturing is active.
///
/// All other write helpers funnel through this function so that capture mode
/// works transparently for every built-in command.
///
/// When the capture buffer is full, additional bytes are silently discarded.
/// Increase `PIPE_BUF_SIZE` if pipelines truncate large output.
#[inline]
pub fn write_bytes(bs: &[u8]) {
    unsafe {
        if PIPE_CAPTURING {
            let avail = PIPE_BUF_SIZE - PIPE_CAPTURE_LEN;
            let n = bs.len().min(avail);
            PIPE_CAPTURE_BUF[PIPE_CAPTURE_LEN..PIPE_CAPTURE_LEN + n]
                .copy_from_slice(&bs[..n]);
            PIPE_CAPTURE_LEN += n;
            return;
        }
    }
    crate::sys::sys_write(1, bs);
}

/// Write a single byte to stdout (or the active pipe buffer).
#[inline]
pub fn write_byte(b: u8) {
    write_bytes(core::slice::from_ref(&b));
}

/// Write a `&str` to stdout (or the active pipe buffer).
#[inline]
pub fn write_str(s: &str) {
    write_bytes(s.as_bytes());
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
