//! Syscall wrappers for RustOS userspace programs.
//!
//! Most syscall wrappers are re-exported from rustos-rt.
//! This module adds shell-specific constants and in-process pipe buffering.

// Re-export syscall constants and wrappers from rustos-rt
pub use rustos_rt::{
    chdir, close, dup2, exec, getcwd, getdents64, open, read_byte, syscall, sys_exit, sys_read,
    sys_write, waitpid, SYS_CHDIR, SYS_CLOSE, SYS_DUP2, SYS_EXEC, SYS_GETCWD, SYS_GETDENTS64,
    SYS_OPEN, SYS_PIPE, SYS_READ, SYS_WAITPID, SYS_WRITE, O_RDONLY,
};

/// Temporary fd used to save stdin while setting up a pipe.
pub const SAVED_STDIN_FD: i64 = 100;
/// Temporary fd used to save stdout while setting up a pipe.
pub const SAVED_STDOUT_FD: i64 = 101;

// ── Shell-specific syscall convenience wrappers ───────────────────────────────

/// Read bytes from an open file descriptor (convenience wrapper).
#[inline]
pub fn read_fd(fd: i64, buf: &mut [u8]) -> i64 {
    sys_read(fd as u64, buf)
}

/// Write bytes to an open file descriptor (convenience wrapper).
#[inline]
pub fn write_fd(fd: i64, data: &[u8]) -> i64 {
    sys_write(fd as u64, data)
}

/// Create a kernel pipe (convenience wrapper with shell-specific name).
/// On success, `pipefd[0]` is the read end and `pipefd[1]` is the write end.
/// Returns 0 on success or a negative error code.
#[inline]
pub fn pipe_kernel(pipefd: &mut [i32; 2]) -> i64 {
    rustos_rt::pipe(pipefd)
}

// ── In-process pipe-stdin buffer ──────────────────────────────────────────────
//
// When the kernel does not support `pipe`/`dup2`, the shell uses an in-process
// buffer to forward the stdout of one built-in command to the stdin of the next.

static mut PIPE_STDIN_BUF: [u8; crate::io::PIPE_BUF_SIZE] =
    [0u8; crate::io::PIPE_BUF_SIZE];
static mut PIPE_STDIN_LEN: usize = 0;
static mut PIPE_STDIN_POS: usize = 0;

/// Load `data` into the pipe-stdin buffer so that the next command can read it.
pub fn pipe_stdin_set(data: &[u8]) {
    unsafe {
        let n = data.len().min(crate::io::PIPE_BUF_SIZE);
        PIPE_STDIN_BUF[..n].copy_from_slice(&data[..n]);
        PIPE_STDIN_LEN = n;
        PIPE_STDIN_POS = 0;
    }
}

/// Clear the pipe-stdin buffer (called after the pipeline finishes).
pub fn pipe_stdin_clear() {
    unsafe {
        PIPE_STDIN_LEN = 0;
        PIPE_STDIN_POS = 0;
    }
}

/// Returns `true` if there are unread bytes in the pipe-stdin buffer.
pub fn pipe_stdin_has_data() -> bool {
    unsafe { PIPE_STDIN_POS < PIPE_STDIN_LEN }
}

/// Read one byte from the pipe-stdin buffer, or `None` if the buffer is empty.
pub fn read_pipe_byte() -> Option<u8> {
    unsafe {
        if PIPE_STDIN_POS < PIPE_STDIN_LEN {
            let b = PIPE_STDIN_BUF[PIPE_STDIN_POS];
            PIPE_STDIN_POS += 1;
            Some(b)
        } else {
            None
        }
    }
}
