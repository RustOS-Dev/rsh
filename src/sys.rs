//! Syscall wrappers for RustOS userspace programs.
//!
//! All syscalls use the `int 0x80` ABI: rax = syscall number,
//! rdi/rsi/rdx = arguments, rax (on return) = result.
//! Positive values indicate success; negative values are error codes.

// ── Syscall numbers ───────────────────────────────────────────────────────────

pub const SYS_READ: u64 = 0;
pub const SYS_WRITE: u64 = 1;
/// Open a file path.
pub const SYS_OPEN: u64 = 2;
/// Close a file descriptor.
pub const SYS_CLOSE: u64 = 3;
/// Create a pipe: fills pipefd[0] (read end) and pipefd[1] (write end).
pub const SYS_PIPE: u64 = 22;
/// Duplicate a file descriptor to a specific number.
pub const SYS_DUP2: u64 = 33;
/// Execute an ELF binary (RustOS-specific: NUL-terminated path in rdi).
pub const SYS_EXEC: u64 = 59;
/// Terminate the process.
pub const SYS_EXIT: u64 = 60;
/// Wait for a child process.
pub const SYS_WAITPID: u64 = 61;
/// Get the current working directory.
pub const SYS_GETCWD: u64 = 79;
/// Change the current working directory.
pub const SYS_CHDIR: u64 = 80;
/// Read directory entries (Linux getdents64 number).
pub const SYS_GETDENTS64: u64 = 217;

/// Temporary fd used to save stdin while setting up a pipe.
pub const SAVED_STDIN_FD: i64 = 100;
/// Temporary fd used to save stdout while setting up a pipe.
pub const SAVED_STDOUT_FD: i64 = 101;

// ── Open flags ────────────────────────────────────────────────────────────────

pub const O_RDONLY: u32 = 0;
const EINVAL: i64 = -22;

#[inline]
fn is_nul_terminated(path: &[u8]) -> bool {
    path.last() == Some(&0)
}

// ── Raw syscall shim ──────────────────────────────────────────────────────────

/// Perform a raw RustOS syscall via `int 0x80`.
///
/// # Safety
/// Caller must ensure the syscall number and arguments are valid.
#[inline(always)]
pub unsafe fn syscall(nr: u64, a0: u64, a1: u64, a2: u64) -> i64 {
    let ret: i64;
    core::arch::asm!(
        "int 0x80",
        inout("rax") nr => ret,
        in("rdi") a0,
        in("rsi") a1,
        in("rdx") a2,
        options(nostack, preserves_flags),
    );
    ret
}

// ── High-level wrappers ───────────────────────────────────────────────────────

/// Write `buf` to file descriptor `fd`.
/// Returns bytes written or a negative error code.
#[inline]
pub fn sys_write(fd: u64, buf: &[u8]) -> i64 {
    unsafe { syscall(SYS_WRITE, fd, buf.as_ptr() as u64, buf.len() as u64) }
}

/// Read up to `buf.len()` bytes from `fd` into `buf`.
/// Returns bytes read or a negative error code.
#[inline]
pub fn sys_read(fd: u64, buf: &mut [u8]) -> i64 {
    unsafe { syscall(SYS_READ, fd, buf.as_mut_ptr() as u64, buf.len() as u64) }
}

/// Terminate the process with `code`.
#[inline]
pub fn sys_exit(code: i64) -> ! {
    unsafe { syscall(SYS_EXIT, code as u64, 0, 0) };
    // The kernel never returns from SYS_EXIT.  This loop satisfies the `-> !`
    // return type without using unsafe `unreachable_unchecked`, which would be
    // undefined behaviour if the kernel ever did return.
    loop {
        core::hint::spin_loop();
    }
}

/// Open `path` (NUL-terminated byte slice).
/// Returns a non-negative fd on success, negative errno on failure.
#[inline]
pub fn open(path: &[u8]) -> i64 {
    if !is_nul_terminated(path) {
        return EINVAL;
    }
    unsafe { syscall(SYS_OPEN, path.as_ptr() as u64, O_RDONLY as u64, 0) }
}

/// Close `fd`.
#[inline]
pub fn close(fd: i64) {
    if fd >= 0 {
        unsafe { syscall(SYS_CLOSE, fd as u64, 0, 0) };
    }
}

/// Execute the ELF binary at `path` (NUL-terminated byte slice).
/// Returns the exit code on success or a negative error code.
#[inline]
pub fn exec(path: &[u8]) -> i64 {
    if !is_nul_terminated(path) {
        return EINVAL;
    }
    unsafe { syscall(SYS_EXEC, path.as_ptr() as u64, 0, 0) }
}

/// Fill `buf` with the current working directory string.
/// Returns the number of bytes written on success, negative on error.
#[inline]
pub fn getcwd(buf: &mut [u8]) -> i64 {
    unsafe { syscall(SYS_GETCWD, buf.as_mut_ptr() as u64, buf.len() as u64, 0) }
}

/// Change the working directory to `path` (NUL-terminated byte slice).
/// Returns 0 on success, negative on error.
#[inline]
pub fn chdir(path: &[u8]) -> i64 {
    if !is_nul_terminated(path) {
        return EINVAL;
    }
    unsafe { syscall(SYS_CHDIR, path.as_ptr() as u64, 0, 0) }
}

/// Read directory entries from `fd` into `buf`.
/// Returns bytes written on success, 0 when exhausted, negative on error.
#[inline]
pub fn getdents64(fd: i64, buf: &mut [u8]) -> i64 {
    unsafe {
        syscall(
            SYS_GETDENTS64,
            fd as u64,
            buf.as_mut_ptr() as u64,
            buf.len() as u64,
        )
    }
}

/// Read bytes from an open file descriptor.
#[inline]
pub fn read_fd(fd: i64, buf: &mut [u8]) -> i64 {
    sys_read(fd as u64, buf)
}

/// Write bytes to an open file descriptor.
#[inline]
pub fn write_fd(fd: i64, data: &[u8]) -> i64 {
    sys_write(fd as u64, data)
}

/// Read a single byte from stdin, busy-waiting until one is available.
///
/// NOTE: The current RustOS kernel returns 0 from `sys_read` (stub).  This
/// function will busy-loop until the kernel implements blocking keyboard reads.
#[inline]
pub fn read_byte() -> u8 {
    let mut b = [0u8; 1];
    loop {
        if sys_read(0, &mut b) > 0 {
            return b[0];
        }
        core::hint::spin_loop();
    }
}

// ── Kernel pipe / dup2 ────────────────────────────────────────────────────────

/// Create a pipe.  On success, `pipefd[0]` is the read end and `pipefd[1]` is
/// the write end.  Returns 0 on success or a negative error code.
#[inline]
pub fn pipe_kernel(pipefd: &mut [i32; 2]) -> i64 {
    unsafe { syscall(SYS_PIPE, pipefd.as_mut_ptr() as u64, 0, 0) }
}

/// Duplicate `oldfd` to `newfd`, closing `newfd` first if necessary.
/// Returns `newfd` on success or a negative error code.
#[inline]
pub fn dup2(oldfd: i64, newfd: i64) -> i64 {
    unsafe { syscall(SYS_DUP2, oldfd as u64, newfd as u64, 0) }
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
