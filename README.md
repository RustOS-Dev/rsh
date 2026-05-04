# rsh — RustOS Shell

A shell for [RustOS](https://github.com/0xnullsect0r/RustOS), inspired by
bash / zsh / fish.  It compiles to a bare-metal ELF executable that the
RustOS kernel loads from the filesystem and runs in userspace via the
[rustos-rt](https://github.com/RustOS-Dev/rustos-rt) runtime.

Install the binary at **`/bin/rsh`** on a RustOS FAT32 volume.

---

## Features

| Feature | Description |
|---------|-------------|
| Line editing | Backspace, Ctrl-U (clear line), Ctrl-C (cancel) |
| History | Up / Down arrow navigation, ring-buffer of 20 entries |
| Tab completion | Completes built-in command names |
| Variable expansion | `$VAR`, `${VAR}`, `$?` (exit status), `$$`, `$0` |
| Quoting | `'literal'`, `"with $expansion"`, `\escape` |
| Variable assignment | `NAME=value` (bare assignment) and `export NAME=VALUE` |
| PATH lookup | Searches `$PATH` for external commands |
| Built-in commands | See table below |

### Built-in commands

| Command | Description |
|---------|-------------|
| `echo [-n] [args…]` | Print arguments |
| `exit [code]` | Exit the shell |
| `clear` | Clear the screen |
| `pwd` | Print working directory |
| `cd [path]` | Change directory |
| `ls [path]` | List directory (requires kernel `SYS_GETDENTS64`) |
| `cat <file>` | Print file contents |
| `exec <path>` | Execute an ELF binary (requires kernel `SYS_EXEC`) |
| `env` | Print all shell variables |
| `export NAME=VALUE` | Set / update a variable |
| `unset NAME` | Remove a variable |
| `history` | Show command history |
| `uname` | Show OS information |
| `type <cmd>` | Show how a command is resolved |
| `true` / `false` | Return exit code 0 / 1 |
| `help` | Show this reference |

### Special variables

| Variable | Default | Meaning |
|----------|---------|---------|
| `PATH` | `/bin` | Colon-separated directories searched for commands |
| `SHELL` | `/bin/rsh` | Path to this shell |
| `HOME` | `/` | Home directory for `cd` with no arguments |
| `PS1` | (coloured prompt) | Prompt string; `\w` = CWD, `\e` = ESC |

---

## Building

### Prerequisites

```bash
rustup toolchain install nightly
rustup component add rust-src --toolchain nightly
```

You also need `rust-lld` (bundled with the nightly toolchain). This project
uses the [rustos-rt](https://github.com/RustOS-Dev/rustos-rt) runtime as a
Git dependency, which provides the ELF entry point, syscall wrappers, and
target specification.

### Compile

```bash
cargo +nightly build --release
```

The `.cargo/config.toml` already sets the target
(`x86_64-unknown-rustos.json`) and enables `build-std`.  The resulting ELF
is at:

```
target/x86_64-unknown-rustos/release/rsh
```

### Install on RustOS

1. Mount a FAT32 drive image (or USB stick) that RustOS will boot from.
2. Create the `/bin` directory on it.
3. Copy the ELF:

```bash
cp target/x86_64-unknown-rustos/release/rsh /mnt/rustos/bin/rsh
```

4. Boot RustOS.  From the kernel's built-in shell, run:

```
exec /bin/rsh
```

---

## Syscall requirements

rsh uses the same `int 0x80` ABI as `rustos-rt` (rax = number, rdi/rsi/rdx =
args, rax = return value).  The numbers below must be handled by the kernel:

| Number | Name | Used by |
|--------|------|---------|
| 0 | `SYS_READ` | stdin input loop; `cat` if opened-file reads are supported |
| 1 | `SYS_WRITE` | all output |
| 2 | `SYS_OPEN` | `cat`, `ls`, external exec search |
| 3 | `SYS_CLOSE` | `cat`, `ls` |
| 59 | `SYS_EXEC` | `exec` built-in, external commands |
| 60 | `SYS_EXIT` | `exit` built-in, Ctrl-D |
| 79 | `SYS_GETCWD` | optional (shell tracks CWD locally) |
| 80 | `SYS_CHDIR` | `cd` (falls back to local tracking) |
| 217 | `SYS_GETDENTS64` | `ls` |

Path syscalls use NUL-terminated path strings, matching the current RustOS
`SYS_OPEN` and `SYS_EXEC` handlers. The current RustOS branch implements
keyboard reads on fd 0, while opened-file reads, `SYS_GETCWD`, `SYS_CHDIR`, and
`SYS_GETDENTS64` may still be unavailable; rsh reports unsupported operations
or falls back to locally tracked state where possible.

---

## Target spec notes

`x86_64-unknown-rustos.json` (provided by the [rustos-rt](https://github.com/RustOS-Dev/rustos-rt) toolchain):

* LLVM target: `x86_64-unknown-none`
* No SSE / MMX (`-mmx,-sse,+soft-float`) — the kernel does not save FPU state
* No red-zone (`disable-redzone: true`)
* Panic strategy: `abort`
* Linker: `rust-lld` with `-Trustos-link.x`
* Load address: `0x0040_0000` (4 MiB) — set by `rustos-link.x`

---

## License

MIT OR Apache-2.0
