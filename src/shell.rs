//! rsh shell: REPL, line editor, built-in commands, and variable expansion.
//!
//! ## Architecture
//!
//! * [`Shell`] owns all mutable state: CWD, environment variables, exit status,
//!   and the [`LineEditor`].
//! * [`LineEditor`] handles character-by-character input from stdin and owns a
//!   [`History`] ring-buffer that supports Up/Down arrow navigation and
//!   tab-completion for built-in commands.
//! * `Shell::execute` expands variables, tokenises the line, and dispatches to
//!   a built-in or to an external binary via `SYS_EXEC`.
//!
//! ## VGA terminal notes
//!
//! The RustOS VGA writer recognises `\n` (newline) and `\x08` (backspace).
//! The line editor uses the classical `\x08 \x08` sequence (BS SPACE BS) to
//! erase characters on screen, matching the convention used by the kernel's own
//! built-in shell.  ANSI CSI escape sequences are sent for colour but will
//! appear as literal bytes on VGA implementations that do not parse them —
//! they can be removed by setting `PS1=rsh:\w$ `.

use crate::{io, sys};

// ── Compile-time tunables ─────────────────────────────────────────────────────

/// Maximum length of a single input line (bytes).
const MAX_LINE: usize = 256;
/// Number of history entries retained.
const HIST_CAP: usize = 20;
/// Maximum length of a history entry (bytes).
const HIST_ENTRY: usize = 128;
/// Maximum length of the current working directory string.
const MAX_CWD: usize = 256;
/// Maximum number of shell variables.
const MAX_VARS: usize = 16;
/// Maximum length of a variable name.
const MAX_VAR_NAME: usize = 32;
/// Maximum length of a variable value.
const MAX_VAR_VAL: usize = 128;
/// Maximum number of command arguments (including command name).
const MAX_ARGS: usize = 32;
/// Maximum length of one argument after expansion.
const MAX_ARG: usize = 128;
/// Maximum length of the rendered prompt.
const MAX_PROMPT: usize = 128;
/// Size of the scratch buffer used while reading directory entries.
const DENTS_BUF: usize = 1024;
/// Size of the file-read scratch buffer used by `cat`.
const CAT_BUF: usize = 512;

pub const VERSION: &str = "0.1.0";

// ── Built-in command names (used for tab completion) ──────────────────────────

const BUILTINS: &[&[u8]] = &[
    b"cat",
    b"cd",
    b"clear",
    b"echo",
    b"env",
    b"exec",
    b"exit",
    b"export",
    b"false",
    b"help",
    b"history",
    b"ls",
    b"pwd",
    b"source",
    b"true",
    b"type",
    b"uname",
    b"unset",
];

// ── History ───────────────────────────────────────────────────────────────────

struct History {
    /// Ring buffer of entries.
    entries: [[u8; HIST_ENTRY]; HIST_CAP],
    /// Length of each entry.
    lens: [usize; HIST_CAP],
    /// Total number of lines ever pushed (may exceed `HIST_CAP`).
    total: usize,
    /// Current browse offset: 0 = live input, 1 = most-recent entry, etc.
    browse: usize,
    /// Snapshot of the live input saved when the user first presses ↑.
    saved: [u8; MAX_LINE],
    saved_len: usize,
}

impl History {
    const fn new() -> Self {
        History {
            entries: [[0u8; HIST_ENTRY]; HIST_CAP],
            lens: [0usize; HIST_CAP],
            total: 0,
            browse: 0,
            saved: [0u8; MAX_LINE],
            saved_len: 0,
        }
    }

    /// Append `line` to history, skipping empty lines and consecutive
    /// duplicates.
    fn push(&mut self, line: &[u8]) {
        let line = trim(line);
        if line.is_empty() {
            return;
        }
        if self.total > 0 {
            let prev = (self.total - 1) % HIST_CAP;
            if self.lens[prev] == line.len() && &self.entries[prev][..line.len()] == line {
                return;
            }
        }
        let idx = self.total % HIST_CAP;
        let copy = line.len().min(HIST_ENTRY);
        self.entries[idx][..copy].copy_from_slice(&line[..copy]);
        self.lens[idx] = copy;
        self.total += 1;
    }

    /// Get entry at `pos` where 1 = most recent, 2 = second-most-recent, etc.
    fn get(&self, pos: usize) -> Option<&[u8]> {
        if pos == 0 || pos > self.total.min(HIST_CAP) {
            return None;
        }
        let idx = (self.total - pos) % HIST_CAP;
        Some(&self.entries[idx][..self.lens[idx]])
    }

    fn len(&self) -> usize {
        self.total.min(HIST_CAP)
    }
}

// ── Line editor ───────────────────────────────────────────────────────────────

struct LineEditor {
    buf: [u8; MAX_LINE],
    len: usize,
    history: History,
}

impl LineEditor {
    const fn new() -> Self {
        LineEditor {
            buf: [0u8; MAX_LINE],
            len: 0,
            history: History::new(),
        }
    }

    /// Read one complete line from stdin.
    ///
    /// Handles:
    /// * Printable ASCII — echoed and appended to the buffer.
    /// * `\x08` / `\x7f` (BS / DEL) — erase last character.
    /// * `\x15` (Ctrl-U) — erase entire line.
    /// * `\x03` (Ctrl-C) — cancel; returns empty slice.
    /// * `\x04` (Ctrl-D) on empty line — synthesises `"exit"`.
    /// * `\x09` (Tab) — tab-complete built-in commands.
    /// * `ESC [ A` / `ESC [ B` — history prev / next.
    /// * `\r` / `\n` — commit the line.
    fn read_line(&mut self, prompt: &[u8]) -> &[u8] {
        self.buf = [0u8; MAX_LINE];
        self.len = 0;
        self.history.browse = 0;
        io::write_bytes(prompt);

        loop {
            let b = sys::read_byte();

            match b {
                // ── Commit ────────────────────────────────────────────────
                b'\r' | b'\n' => {
                    io::write_byte(b'\n');
                    return &self.buf[..self.len];
                }

                // ── Escape sequences (arrows, etc.) ───────────────────────
                0x1b => {
                    let b2 = sys::read_byte();
                    if b2 == b'[' {
                        let b3 = sys::read_byte();
                        match b3 {
                            b'A' => self.history_prev(prompt),
                            b'B' => self.history_next(prompt),
                            // Right / Left arrows — ignored in append-only mode.
                            b'C' | b'D' => {}
                            // Delete key (ESC [ 3 ~): consume trailing ~.
                            b'3' => {
                                let _ = sys::read_byte();
                            }
                            _ => {}
                        }
                    }
                    // SS3 sequences (Home/End on some terminals): consume char.
                    // else: ignore unknown escape.
                }

                // ── Backspace / DEL ───────────────────────────────────────
                0x08 | 0x7f => {
                    if self.len > 0 {
                        self.len -= 1;
                        // BS SPACE BS: move back, overwrite with space, move back.
                        io::write_bytes(b"\x08 \x08");
                    }
                }

                // ── Ctrl-U: erase line ────────────────────────────────────
                0x15 => {
                    for _ in 0..self.len {
                        io::write_bytes(b"\x08 \x08");
                    }
                    self.len = 0;
                }

                // ── Ctrl-C: cancel ────────────────────────────────────────
                0x03 => {
                    io::write_bytes(b"^C\n");
                    self.buf = [0u8; MAX_LINE];
                    self.len = 0;
                    return &self.buf[..0];
                }

                // ── Ctrl-D: EOF on empty line ─────────────────────────────
                0x04 => {
                    if self.len == 0 {
                        io::write_bytes(b"exit\n");
                        self.buf[..4].copy_from_slice(b"exit");
                        self.len = 4;
                        return &self.buf[..4];
                    }
                }

                // ── Tab: complete built-in ────────────────────────────────
                0x09 => {
                    self.tab_complete(prompt);
                }

                // ── Printable ASCII ───────────────────────────────────────
                c if c >= 0x20 && c < 0x7f => {
                    if self.len < MAX_LINE - 1 {
                        self.buf[self.len] = c;
                        self.len += 1;
                        io::write_byte(c);
                    }
                }

                // ── Everything else: ignore ───────────────────────────────
                _ => {}
            }
        }
    }

    /// Replace the current input with the previous history entry.
    fn history_prev(&mut self, _prompt: &[u8]) {
        if self.history.browse == 0 {
            // Save live input before starting to browse.
            self.history.saved_len = self.len;
            self.history.saved[..self.len].copy_from_slice(&self.buf[..self.len]);
        }
        let new_pos = self.history.browse + 1;
        // Copy entry to a local buffer to avoid the borrow conflict with
        // `self.erase_input()` (which mutably borrows `self`).
        let mut tmp = [0u8; HIST_ENTRY];
        let tmp_len = match self.history.get(new_pos) {
            Some(entry) => {
                let n = entry.len().min(HIST_ENTRY);
                tmp[..n].copy_from_slice(&entry[..n]);
                n
            }
            None => return,
        };
        self.erase_input();
        let copy = tmp_len.min(MAX_LINE - 1);
        self.buf[..copy].copy_from_slice(&tmp[..copy]);
        self.len = copy;
        io::write_bytes(&self.buf[..self.len]);
        self.history.browse = new_pos;
    }

    /// Replace the current input with the next (more recent) history entry or
    /// restore the saved live input.
    fn history_next(&mut self, _prompt: &[u8]) {
        if self.history.browse == 0 {
            return;
        }
        let new_pos = self.history.browse - 1;
        // Copy the target entry before mutably borrowing self.
        let mut tmp = [0u8; MAX_LINE];
        let tmp_len = if new_pos == 0 {
            let n = self.history.saved_len.min(MAX_LINE);
            tmp[..n].copy_from_slice(&self.history.saved[..n]);
            n
        } else {
            match self.history.get(new_pos) {
                Some(entry) => {
                    let n = entry.len().min(MAX_LINE);
                    tmp[..n].copy_from_slice(&entry[..n]);
                    n
                }
                None => 0,
            }
        };
        self.erase_input();
        let copy = tmp_len.min(MAX_LINE - 1);
        self.buf[..copy].copy_from_slice(&tmp[..copy]);
        self.len = copy;
        io::write_bytes(&self.buf[..self.len]);
        self.history.browse = new_pos;
    }

    /// Erase `self.len` characters from the terminal using BS SPACE BS sequences.
    fn erase_input(&mut self) {
        for _ in 0..self.len {
            io::write_bytes(b"\x08 \x08");
        }
        self.len = 0;
    }

    /// Tab-complete the current input against the built-in command list.
    fn tab_complete(&mut self, prompt: &[u8]) {
        let prefix = &self.buf[..self.len];
        let mut matches = [0u8; 16];
        let mut count = 0usize;

        for (i, cmd) in BUILTINS.iter().enumerate() {
            if cmd.len() >= prefix.len() && &cmd[..prefix.len()] == prefix {
                if count < 16 {
                    matches[count] = i as u8;
                    count += 1;
                }
            }
        }

        match count {
            0 => {} // no match — do nothing
            1 => {
                let cmd = BUILTINS[matches[0] as usize];
                self.erase_input();
                let copy = cmd.len().min(MAX_LINE - 2);
                self.buf[..copy].copy_from_slice(&cmd[..copy]);
                self.len = copy;
                // Append a trailing space for convenience.
                if self.len < MAX_LINE - 1 {
                    self.buf[self.len] = b' ';
                    self.len += 1;
                }
                io::write_bytes(&self.buf[..self.len]);
            }
            _ => {
                // Show all matches, then redisplay the prompt + current input.
                io::write_byte(b'\n');
                for i in 0..count {
                    io::write_bytes(BUILTINS[matches[i] as usize]);
                    io::write_bytes(b"  ");
                }
                io::write_byte(b'\n');
                io::write_bytes(prompt);
                io::write_bytes(&self.buf[..self.len]);
            }
        }
    }
}

// ── Environment variable table ────────────────────────────────────────────────

#[derive(Clone, Copy)]
struct Var {
    name: [u8; MAX_VAR_NAME],
    name_len: usize,
    value: [u8; MAX_VAR_VAL],
    value_len: usize,
}

impl Var {
    const fn empty() -> Self {
        Var {
            name: [0u8; MAX_VAR_NAME],
            name_len: 0,
            value: [0u8; MAX_VAR_VAL],
            value_len: 0,
        }
    }
}

// ── Shell ─────────────────────────────────────────────────────────────────────

pub struct Shell {
    editor: LineEditor,
    cwd: [u8; MAX_CWD],
    cwd_len: usize,
    last_status: i64,
    vars: [Var; MAX_VARS],
    var_count: usize,
    running: bool,
}

impl Shell {
    fn new() -> Self {
        let mut sh = Shell {
            editor: LineEditor::new(),
            cwd: [0u8; MAX_CWD],
            cwd_len: 1,
            last_status: 0,
            vars: [Var::empty(); MAX_VARS],
            var_count: 0,
            running: true,
        };
        sh.cwd[0] = b'/';
        // Default environment variables.
        sh.set_var(b"PATH", b"/bin");
        sh.set_var(b"SHELL", b"/bin/rsh");
        sh.set_var(b"HOME", b"/");
        // Default prompt: "rsh:<cwd>$ "  (\e sequences give colour on
        // terminals that support ANSI; on plain VGA they appear as-is and
        // can be overridden with `export PS1=rsh:\\w$ `).
        sh.set_var(
            b"PS1",
            b"\x1b[1;32mrsh\x1b[0m:\x1b[1;34m\\w\x1b[0m$ ",
        );
        sh
    }

    // ── Variable access ───────────────────────────────────────────────────

    fn get_var(&self, name: &[u8]) -> Option<&[u8]> {
        for i in 0..self.var_count {
            let v = &self.vars[i];
            if v.name_len == name.len() && &v.name[..name.len()] == name {
                return Some(&v.value[..v.value_len]);
            }
        }
        None
    }

    fn set_var(&mut self, name: &[u8], value: &[u8]) {
        for i in 0..self.var_count {
            if self.vars[i].name_len == name.len()
                && &self.vars[i].name[..name.len()] == name
            {
                let vl = value.len().min(MAX_VAR_VAL);
                self.vars[i].value[..vl].copy_from_slice(&value[..vl]);
                self.vars[i].value_len = vl;
                return;
            }
        }
        if self.var_count < MAX_VARS {
            let i = self.var_count;
            let nl = name.len().min(MAX_VAR_NAME);
            self.vars[i].name[..nl].copy_from_slice(&name[..nl]);
            self.vars[i].name_len = nl;
            let vl = value.len().min(MAX_VAR_VAL);
            self.vars[i].value[..vl].copy_from_slice(&value[..vl]);
            self.vars[i].value_len = vl;
            self.var_count += 1;
        }
    }

    fn unset_var(&mut self, name: &[u8]) {
        for i in 0..self.var_count {
            if self.vars[i].name_len == name.len()
                && &self.vars[i].name[..name.len()] == name
            {
                for j in i..self.var_count - 1 {
                    self.vars[j] = self.vars[j + 1];
                }
                self.var_count -= 1;
                return;
            }
        }
    }

    // ── Prompt formatting ─────────────────────────────────────────────────

    /// Render the PS1 prompt into `out`, expanding `\w` (CWD) and `\e`
    /// (ESC).  Returns the number of bytes written.
    fn format_prompt(&self, out: &mut [u8; MAX_PROMPT]) -> usize {
        let ps1 = self.get_var(b"PS1").unwrap_or(b"rsh:\\w$ ");
        let mut i = 0usize;
        let mut j = 0usize;
        while i < ps1.len() && j < MAX_PROMPT {
            if ps1[i] == b'\\' && i + 1 < ps1.len() {
                i += 1;
                match ps1[i] {
                    b'w' => {
                        // Insert CWD.
                        let n = self.cwd_len.min(MAX_PROMPT - j);
                        out[j..j + n].copy_from_slice(&self.cwd[..n]);
                        j += n;
                    }
                    b'e' => {
                        if j < MAX_PROMPT {
                            out[j] = 0x1b;
                            j += 1;
                        }
                    }
                    b'n' => {
                        if j < MAX_PROMPT {
                            out[j] = b'\n';
                            j += 1;
                        }
                    }
                    b'$' => {
                        if j < MAX_PROMPT {
                            out[j] = b'$';
                            j += 1;
                        }
                    }
                    b'\\' => {
                        if j < MAX_PROMPT {
                            out[j] = b'\\';
                            j += 1;
                        }
                    }
                    c => {
                        if j < MAX_PROMPT {
                            out[j] = b'\\';
                            j += 1;
                        }
                        if j < MAX_PROMPT {
                            out[j] = c;
                            j += 1;
                        }
                    }
                }
                i += 1;
            } else {
                out[j] = ps1[i];
                j += 1;
                i += 1;
            }
        }
        j
    }

    // ── Command parsing ───────────────────────────────────────────────────

    /// Expand variables in `input`, storing the result in `out`.
    /// Handles `$VAR`, `${VAR}`, `$?`, `$$`, `$0`; single-quoted and
    /// double-quoted regions; and `\` escapes.
    /// Returns the number of bytes written to `out`.
    fn expand_vars(&self, input: &[u8], out: &mut [u8]) -> usize {
        let mut i = 0usize;
        let mut j = 0usize;
        let mut in_sq = false; // inside single quotes

        macro_rules! emit {
            ($b:expr) => {
                if j < out.len() {
                    out[j] = $b;
                    j += 1;
                }
            };
        }

        while i < input.len() {
            let c = input[i];

            if in_sq {
                if c == b'\'' {
                    in_sq = false;
                } else {
                    emit!(c);
                }
                i += 1;
                continue;
            }

            match c {
                b'\'' => {
                    in_sq = true;
                    i += 1;
                }
                b'"' => {
                    // Double-quoted: pass through with variable expansion.
                    i += 1;
                    while i < input.len() && input[i] != b'"' {
                        if input[i] == b'\\' && i + 1 < input.len() {
                            i += 1;
                            emit!(input[i]);
                            i += 1;
                        } else if input[i] == b'$' {
                            i += 1;
                            i = self.expand_dollar(input, i, out, &mut j);
                        } else {
                            emit!(input[i]);
                            i += 1;
                        }
                    }
                    if i < input.len() {
                        i += 1; // closing "
                    }
                }
                b'\\' => {
                    i += 1;
                    if i < input.len() {
                        emit!(input[i]);
                        i += 1;
                    }
                }
                b'$' => {
                    i += 1;
                    i = self.expand_dollar(input, i, out, &mut j);
                }
                _ => {
                    emit!(c);
                    i += 1;
                }
            }
        }
        j
    }

    /// Expand a `$…` expression starting at `input[i]`.
    /// Returns the new value of `i` (after the expansion token).
    fn expand_dollar(&self, input: &[u8], mut i: usize, out: &mut [u8], j: &mut usize) -> usize {
        macro_rules! emit {
            ($b:expr) => {
                if *j < out.len() {
                    out[*j] = $b;
                    *j += 1;
                }
            };
        }
        macro_rules! emit_slice {
            ($s:expr) => {
                for &b in $s {
                    emit!(b);
                }
            };
        }

        match input.get(i) {
            Some(&b'?') => {
                // $? — last exit status.
                i += 1;
                let mut tmp = [0u8; 21];
                let s = crate::io::fmt_i64(self.last_status, &mut tmp);
                emit_slice!(s);
            }
            Some(&b'$') => {
                // $$ — process name.
                i += 1;
                emit_slice!(b"rsh");
            }
            Some(&b'0') => {
                // $0 — shell path.
                i += 1;
                emit_slice!(b"/bin/rsh");
            }
            Some(&b'{') => {
                // ${VAR}
                i += 1; // skip '{'
                let start = i;
                while i < input.len() && input[i] != b'}' {
                    i += 1;
                }
                let name = &input[start..i];
                if i < input.len() {
                    i += 1; // skip '}'
                }
                if let Some(val) = self.get_var(name) {
                    emit_slice!(val);
                }
            }
            Some(&c) if c.is_ascii_alphanumeric() || c == b'_' => {
                // $VAR
                let start = i;
                while i < input.len()
                    && (input[i].is_ascii_alphanumeric() || input[i] == b'_')
                {
                    i += 1;
                }
                let name = &input[start..i];
                if let Some(val) = self.get_var(name) {
                    emit_slice!(val);
                }
            }
            _ => {
                // Bare `$` — emit literally.
                emit!(b'$');
            }
        }
        i
    }

    /// Parse `line` into tokens.  Each token is stored in `args.data[n]` with
    /// its length in `args.lens[n]`.  Returns the token count.
    ///
    /// Processes quoting and escaping that survived `expand_vars`.  (After
    /// expansion, `'…'` regions are already stripped, but `"…"` can still
    /// contain spaces that should not split tokens.)
    fn tokenize<'a>(&self, line: &[u8], args: &'a mut Args) -> usize {
        let mut i = 0usize;
        let mut count = 0usize;

        // Skip leading whitespace.
        while i < line.len() && line[i].is_ascii_whitespace() {
            i += 1;
        }

        while i < line.len() && count < MAX_ARGS {
            let mut ai = 0usize; // write pos within current arg

            macro_rules! arg_push {
                ($b:expr) => {
                    if ai < MAX_ARG {
                        args.data[count][ai] = $b;
                        ai += 1;
                    }
                };
            }

            // Accumulate one token until unquoted whitespace or end.
            loop {
                if i >= line.len() {
                    break;
                }
                let c = line[i];
                if c.is_ascii_whitespace() {
                    break;
                }
                match c {
                    b'\\' => {
                        i += 1;
                        if i < line.len() {
                            arg_push!(line[i]);
                            i += 1;
                        }
                    }
                    _ => {
                        arg_push!(c);
                        i += 1;
                    }
                }
            }

            if ai > 0 {
                args.lens[count] = ai;
                count += 1;
            }

            // Skip trailing whitespace between tokens.
            while i < line.len() && line[i].is_ascii_whitespace() {
                i += 1;
            }
        }

        count
    }

    // ── REPL ──────────────────────────────────────────────────────────────

    /// Execute one command line (after splitting history-push from the borrow).
    fn execute(&mut self, line: &[u8]) {
        let line = trim(line);
        if line.is_empty() || line[0] == b'#' {
            return;
        }

        // Handle `NAME=value` variable assignment (no command follows).
        if let Some(eq) = line.iter().position(|&b| b == b'=') {
            let name = &line[..eq];
            if !name.is_empty()
                && name.iter().all(|&b| b.is_ascii_alphanumeric() || b == b'_')
            {
                let value = &line[eq + 1..];
                // Expand variables in the RHS.
                let mut exp = [0u8; MAX_VAR_VAL];
                let explen = self.expand_vars(value, &mut exp);
                self.set_var(name, &exp[..explen]);
                return;
            }
        }

        // Expand variables.
        let mut expanded = [0u8; MAX_LINE * 2];
        let explen = self.expand_vars(line, &mut expanded);
        let expanded = &expanded[..explen];

        // Tokenise.
        let mut args = Args::new();
        let argc = self.tokenize(expanded, &mut args);
        if argc == 0 {
            return;
        }

        let cmd = &args.data[0][..args.lens[0]];

        // Build a slice of `&[u8]` argument views (skipping argv[0]).
        let mut argv: [&[u8]; MAX_ARGS] = [b""; MAX_ARGS];
        for k in 1..argc {
            argv[k - 1] = &args.data[k][..args.lens[k]];
        }
        let argv = &argv[..argc - 1];

        self.dispatch(cmd, argv);
    }

    fn dispatch(&mut self, cmd: &[u8], args: &[&[u8]]) {
        match cmd {
            b"echo" => self.cmd_echo(args),
            b"exit" => self.cmd_exit(args),
            b"clear" => self.cmd_clear(),
            b"help" => self.cmd_help(),
            b"history" => self.cmd_history(),
            b"pwd" => self.cmd_pwd(),
            b"cd" => self.cmd_cd(args),
            b"ls" => self.cmd_ls(args),
            b"cat" => self.cmd_cat(args),
            b"env" => self.cmd_env(),
            b"export" => self.cmd_export(args),
            b"unset" => self.cmd_unset(args),
            b"exec" => self.cmd_exec(args),
            b"uname" => self.cmd_uname(),
            b"true" => {
                self.last_status = 0;
            }
            b"false" => {
                self.last_status = 1;
            }
            b"type" => self.cmd_type(args),
            b"source" => self.cmd_source(args),
            _ => self.cmd_external(cmd, args),
        }
    }

    // ── Built-in commands ─────────────────────────────────────────────────

    fn cmd_echo(&mut self, args: &[&[u8]]) {
        let mut newline = true;
        let mut start = 0;
        if args.first().copied() == Some(b"-n") {
            newline = false;
            start = 1;
        }
        for (i, arg) in args[start..].iter().enumerate() {
            if i > 0 {
                io::write_byte(b' ');
            }
            io::write_bytes(arg);
        }
        if newline {
            io::write_byte(b'\n');
        }
        self.last_status = 0;
    }

    fn cmd_exit(&mut self, args: &[&[u8]]) {
        let code = args
            .first()
            .and_then(|a| parse_i64(a))
            .unwrap_or(self.last_status);
        crate::sys::sys_exit(code);
    }

    fn cmd_clear(&mut self) {
        // Send ANSI clear-screen + cursor home for terminals that support it.
        // On raw VGA without ANSI parsing, print many newlines instead.
        io::write_bytes(b"\x1b[2J\x1b[H");
        // Fallback: also print newlines so at least the screen scrolls up.
        for _ in 0..25 {
            io::write_byte(b'\n');
        }
        self.last_status = 0;
    }

    fn cmd_help(&mut self) {
        println!("rsh {} — RustOS Shell", VERSION);
        println!();
        println!("Built-in commands:");
        println!("  echo [-n] [args...]    Print text");
        println!("  exit [code]            Exit the shell");
        println!("  clear                  Clear the screen");
        println!("  pwd                    Print current directory");
        println!("  cd [path]              Change directory");
        println!("  ls [path]              List directory");
        println!("  cat <file>             Print file contents");
        println!("  exec <path>            Execute an ELF binary");
        println!("  env                    Show environment variables");
        println!("  export NAME=VALUE      Set/export a variable");
        println!("  unset NAME             Remove a variable");
        println!("  history                Show command history");
        println!("  uname                  Show OS information");
        println!("  type <cmd>             Show how a command is resolved");
        println!("  true / false           Return 0 / 1");
        println!("  help                   Show this message");
        println!();
        println!("Special variables:  $?  $0  $PATH  $HOME  $SHELL  $PS1");
        println!("Quoting:            '...'  \"...\"  \\<char>");
        println!("Keyboard:           Tab  ↑/↓  Ctrl-C  Ctrl-U  Ctrl-D");
        self.last_status = 0;
    }

    fn cmd_history(&mut self) {
        let n = self.editor.history.len();
        for pos in (1..=n).rev() {
            if let Some(entry) = self.editor.history.get(pos) {
                let idx = n + 1 - pos;
                let mut nbuf = [0u8; 20];
                let ns = crate::io::fmt_u64(idx as u64, &mut nbuf);
                // Right-align the index in a 4-wide column, then two spaces.
                let pad = 4usize.saturating_sub(ns.len());
                for _ in 0..pad {
                    io::write_byte(b' ');
                }
                io::write_bytes(ns);
                io::write_bytes(b"  ");
                io::write_bytes(entry);
                io::write_byte(b'\n');
            }
        }
        self.last_status = 0;
    }

    fn cmd_pwd(&mut self) {
        io::write_bytes(&self.cwd[..self.cwd_len]);
        io::write_byte(b'\n');
        self.last_status = 0;
    }

    fn cmd_cd(&mut self, args: &[&[u8]]) {
        let target = args
            .first()
            .copied()
            .unwrap_or_else(|| self.get_var(b"HOME").unwrap_or(b"/"));

        // Resolve to an absolute path.
        let mut new_cwd = [0u8; MAX_CWD];
        let new_len = resolve_path(&self.cwd[..self.cwd_len], target, &mut new_cwd);

        // Try the kernel syscall.
        let r = sys::chdir(&new_cwd[..new_len]);
        if r >= 0 || r == -38 {
            // -38 = ENOSYS (kernel stub not yet implemented) — still update
            // the shell's local CWD so `pwd` and relative paths work.
            self.cwd[..new_len].copy_from_slice(&new_cwd[..new_len]);
            self.cwd_len = new_len;
            self.last_status = 0;
        } else {
            println!("cd: {}: no such directory", as_str(target));
            self.last_status = 1;
        }
    }

    fn cmd_ls(&mut self, args: &[&[u8]]) {
        let path_arg = args.first().copied().unwrap_or(b".");
        let mut path = [0u8; MAX_CWD];
        let path_len = resolve_path(&self.cwd[..self.cwd_len], path_arg, &mut path);
        let path = &path[..path_len];

        // Try to open as a directory.
        let mut path_nul = [0u8; MAX_CWD + 1];
        path_nul[..path_len].copy_from_slice(path);
        path_nul[path_len] = 0;

        let fd = sys::open(&path_nul[..path_len + 1]);
        if fd < 0 {
            println!("ls: cannot open '{}'", as_str(path));
            self.last_status = 1;
            return;
        }

        let mut buf = [0u8; DENTS_BUF];
        let n = sys::getdents64(fd, &mut buf);
        sys::close(fd);

        if n < 0 {
            // getdents64 not implemented by the kernel yet — inform the user.
            println!("ls: not supported by this kernel (SYS_GETDENTS64 stub)");
            self.last_status = 1;
            return;
        }
        if n == 0 {
            // Empty directory.
            self.last_status = 0;
            return;
        }

        // Parse Linux dirent64 records: ino(8) off(8) reclen(2) type(1) name(…NUL).
        let mut off = 0usize;
        while off + 19 < n as usize {
            let reclen = u16::from_le_bytes([buf[off + 16], buf[off + 17]]) as usize;
            if reclen == 0 {
                break;
            }
            let name_start = off + 19;
            if name_start < off + reclen {
                let name_slice = &buf[name_start..off + reclen];
                let name_end = name_slice
                    .iter()
                    .position(|&b| b == 0)
                    .unwrap_or(name_slice.len());
                let name = &name_slice[..name_end];
                let file_type = buf[off + 18];
                io::write_bytes(name);
                // 4 = DT_DIR
                if file_type == 4 {
                    io::write_byte(b'/');
                }
                io::write_byte(b'\n');
            }
            off += reclen;
        }
        self.last_status = 0;
    }

    fn cmd_cat(&mut self, args: &[&[u8]]) {
        if args.is_empty() {
            println!("cat: usage: cat <file>");
            self.last_status = 1;
            return;
        }
        let mut any_error = false;
        for &arg in args {
            let mut path = [0u8; MAX_CWD + 1];
            let path_len = resolve_path(&self.cwd[..self.cwd_len], arg, &mut path);
            path[path_len] = 0;

            let fd = sys::open(&path[..path_len + 1]);
            if fd < 0 {
                println!("cat: {}: cannot open", as_str(arg));
                any_error = true;
                continue;
            }
            let mut buf = [0u8; CAT_BUF];
            loop {
                let n = sys::read_fd(fd, &mut buf);
                if n <= 0 {
                    break;
                }
                io::write_bytes(&buf[..n as usize]);
            }
            sys::close(fd);
        }
        self.last_status = if any_error { 1 } else { 0 };
    }

    fn cmd_env(&mut self) {
        for i in 0..self.var_count {
            let v = &self.vars[i];
            io::write_bytes(&v.name[..v.name_len]);
            io::write_byte(b'=');
            io::write_bytes(&v.value[..v.value_len]);
            io::write_byte(b'\n');
        }
        self.last_status = 0;
    }

    fn cmd_export(&mut self, args: &[&[u8]]) {
        for &arg in args {
            if let Some(eq) = arg.iter().position(|&b| b == b'=') {
                let name = &arg[..eq];
                let value = &arg[eq + 1..];
                let mut exp = [0u8; MAX_VAR_VAL];
                let explen = self.expand_vars(value, &mut exp);
                self.set_var(name, &exp[..explen]);
            } else {
                // `export NAME` — just ensure it exists (no-op if already set).
                if self.get_var(arg).is_none() {
                    self.set_var(arg, b"");
                }
            }
        }
        self.last_status = 0;
    }

    fn cmd_unset(&mut self, args: &[&[u8]]) {
        for &arg in args {
            self.unset_var(arg);
        }
        self.last_status = 0;
    }

    fn cmd_exec(&mut self, args: &[&[u8]]) {
        let Some(&path_arg) = args.first() else {
            println!("exec: usage: exec <path>");
            self.last_status = 1;
            return;
        };
        let mut path = [0u8; MAX_CWD + 1];
        let path_len = if path_arg[0] == b'/' {
            // Absolute path.
            let n = path_arg.len().min(MAX_CWD);
            path[..n].copy_from_slice(&path_arg[..n]);
            n
        } else {
            // Search PATH.
            resolve_path(&self.cwd[..self.cwd_len], path_arg, &mut path)
        };
        path[path_len] = 0;

        let code = sys::exec(&path[..path_len + 1]);
        if code < 0 {
            println!("exec: {}: failed (error {})", as_str(path_arg), code);
            self.last_status = 127;
        } else {
            self.last_status = code;
        }
    }

    fn cmd_uname(&mut self) {
        println!("RustOS  rsh {}  x86_64", VERSION);
        self.last_status = 0;
    }

    fn cmd_type(&mut self, args: &[&[u8]]) {
        let mut any_missing = false;
        for &arg in args {
            if BUILTINS.iter().any(|&b| b == arg) {
                print!("{} is a shell built-in\n", as_str(arg));
            } else {
                let mut full = [0u8; MAX_CWD + 1];
                if let Some(full_len) = self.find_in_path(arg, &mut full) {
                    print!("{} is {}\n", as_str(arg), as_str(&full[..full_len]));
                } else {
                    println!("{}: not found", as_str(arg));
                    any_missing = true;
                }
            }
        }
        if !args.is_empty() {
            self.last_status = if any_missing { 1 } else { 0 };
        }
    }

    fn cmd_source(&mut self, _args: &[&[u8]]) {
        // `source` / `.` : read and execute commands from a file.
        // Not yet implemented (requires file read + line-by-line execution).
        println!("source: not yet implemented");
        self.last_status = 1;
    }

    /// Search `$PATH` for `cmd`, writing the matched full path into `out`.
    /// Returns the matched path length on success.
    fn find_in_path(&self, cmd: &[u8], out: &mut [u8; MAX_CWD + 1]) -> Option<usize> {
        let path_val = self.get_var(b"PATH")?;
        let mut tmp = [0u8; MAX_VAR_VAL];
        let pv_len = path_val.len().min(MAX_VAR_VAL);
        tmp[..pv_len].copy_from_slice(&path_val[..pv_len]);
        let pv = &tmp[..pv_len];

        for dir in split_colon(pv) {
            let fl = join_path(dir, cmd, out);
            if fl >= out.len() {
                continue;
            }
            out[fl] = 0;
            let fd = sys::open(&out[..fl + 1]);
            if fd >= 0 {
                sys::close(fd);
                return Some(fl);
            }
        }
        None
    }

    fn cmd_external(&mut self, cmd: &[u8], _args: &[&[u8]]) {
        // Search PATH for the command.
        let mut found_path = [0u8; MAX_CWD + 1];
        let found_len = if cmd.first() == Some(&b'/') {
            // Absolute path — use directly.
            let n = cmd.len().min(MAX_CWD);
            found_path[..n].copy_from_slice(&cmd[..n]);
            Some(n)
        } else {
            self.find_in_path(cmd, &mut found_path)
        };

        let Some(found_len) = found_len else {
            println!("{}: command not found", as_str(cmd));
            self.last_status = 127;
            return;
        };

        found_path[found_len] = 0;
        let code = sys::exec(&found_path[..found_len + 1]);
        if code < 0 {
            println!("{}: exec failed ({})", as_str(cmd), code);
            self.last_status = 127;
        } else {
            self.last_status = code;
        }
    }
}

// ── Public entry point ────────────────────────────────────────────────────────

/// Run the shell REPL.  Returns when the user exits.
pub fn run() -> i64 {
    let mut shell = Shell::new();

    println!("\x1b[1;32mrsh\x1b[0m {} — RustOS Shell", VERSION);
    println!("Type 'help' for available commands, 'exit' to quit.\n");

    let mut line_buf = [0u8; MAX_LINE];

    loop {
        if !shell.running {
            break;
        }

        // Build prompt.
        let mut prompt_buf = [0u8; MAX_PROMPT];
        let prompt_len = shell.format_prompt(&mut prompt_buf);
        let prompt = &prompt_buf[..prompt_len];

        // Read a line (the line is owned by the editor's buffer).
        let line_ref = shell.editor.read_line(prompt);
        let line_len = line_ref.len();

        // Copy into a local buffer so we can use `shell` again below.
        let copy_len = line_len.min(MAX_LINE);
        line_buf[..copy_len].copy_from_slice(&line_ref[..copy_len]);

        if copy_len == 0 {
            continue;
        }

        // Add to history before executing.
        shell.editor.history.push(&line_buf[..copy_len]);

        shell.execute(&line_buf[..copy_len]);
    }

    shell.last_status
}

// ── Argument storage ──────────────────────────────────────────────────────────

struct Args {
    data: [[u8; MAX_ARG]; MAX_ARGS],
    lens: [usize; MAX_ARGS],
}

impl Args {
    fn new() -> Self {
        Args {
            data: [[0u8; MAX_ARG]; MAX_ARGS],
            lens: [0usize; MAX_ARGS],
        }
    }
}

// ── Path utilities ────────────────────────────────────────────────────────────

/// Resolve `rel` relative to `base`, writing the normalised absolute path
/// into `out`.  Returns the number of bytes written.
fn resolve_path(base: &[u8], rel: &[u8], out: &mut [u8]) -> usize {
    let mut tmp = [0u8; MAX_CWD * 2];

    let len = if rel.first() == Some(&b'/') {
        // Absolute path.
        let n = rel.len().min(tmp.len());
        tmp[..n].copy_from_slice(&rel[..n]);
        n
    } else {
        // Relative path: start from base.
        let n = base.len().min(tmp.len());
        tmp[..n].copy_from_slice(&base[..n]);
        let mut l = n;
        if l > 0 && tmp[l - 1] != b'/' {
            if l < tmp.len() {
                tmp[l] = b'/';
                l += 1;
            }
        }
        let rn = rel.len().min(tmp.len() - l);
        tmp[l..l + rn].copy_from_slice(&rel[..rn]);
        l + rn
    };

    normalize_path(&tmp[..len], out)
}

/// Normalise a path by resolving `.` and `..` components.
/// Writes the result into `out` and returns the number of bytes written.
fn normalize_path(path: &[u8], out: &mut [u8]) -> usize {
    // Split on '/', process each component.
    let mut stack = [[0u8; 64]; 32];
    let mut stack_lens = [0usize; 32];
    let mut depth = 0usize;

    let mut i = 0usize;
    while i <= path.len() {
        // Find next separator or end.
        let start = i;
        while i < path.len() && path[i] != b'/' {
            i += 1;
        }
        let seg = &path[start..i];
        if i < path.len() {
            i += 1; // skip '/'
        }

        match seg {
            b"" | b"." => {} // skip
            b".." => {
                if depth > 0 {
                    depth -= 1;
                }
            }
            s => {
                if depth < 32 {
                    let n = s.len().min(64);
                    stack[depth][..n].copy_from_slice(&s[..n]);
                    stack_lens[depth] = n;
                    depth += 1;
                }
            }
        }
    }

    // Reconstruct path.
    let mut j = 0usize;
    if j < out.len() {
        out[j] = b'/';
        j += 1;
    }
    for d in 0..depth {
        let n = stack_lens[d].min(out.len().saturating_sub(j));
        out[j..j + n].copy_from_slice(&stack[d][..n]);
        j += n;
        if d + 1 < depth && j < out.len() {
            out[j] = b'/';
            j += 1;
        }
    }
    j
}

/// Join `dir` and `name` with a `/` separator into `out`.
/// Returns the number of bytes written.
fn join_path(dir: &[u8], name: &[u8], out: &mut [u8]) -> usize {
    let dn = dir.len().min(out.len());
    out[..dn].copy_from_slice(&dir[..dn]);
    let mut j = dn;
    if j < out.len() && (j == 0 || out[j - 1] != b'/') {
        out[j] = b'/';
        j += 1;
    }
    let nn = name.len().min(out.len() - j);
    out[j..j + nn].copy_from_slice(&name[..nn]);
    j + nn
}

/// Iterate over colon-separated segments in `path`.
struct SplitColon<'a> {
    data: &'a [u8],
    pos: usize,
}

fn split_colon(s: &[u8]) -> SplitColon<'_> {
    SplitColon { data: s, pos: 0 }
}

impl<'a> Iterator for SplitColon<'a> {
    type Item = &'a [u8];
    fn next(&mut self) -> Option<Self::Item> {
        if self.pos >= self.data.len() {
            return None;
        }
        let start = self.pos;
        while self.pos < self.data.len() && self.data[self.pos] != b':' {
            self.pos += 1;
        }
        let seg = &self.data[start..self.pos];
        if self.pos < self.data.len() {
            self.pos += 1; // skip ':'
        }
        if seg.is_empty() {
            self.next() // skip empty segments
        } else {
            Some(seg)
        }
    }
}

// ── Miscellaneous helpers ─────────────────────────────────────────────────────

/// Trim leading and trailing ASCII whitespace.
fn trim(s: &[u8]) -> &[u8] {
    let s = match s.iter().position(|b| !b.is_ascii_whitespace()) {
        Some(i) => &s[i..],
        None => return &[],
    };
    match s.iter().rposition(|b| !b.is_ascii_whitespace()) {
        Some(i) => &s[..i + 1],
        None => &[],
    }
}

/// Parse a decimal integer from a byte slice.
fn parse_i64(s: &[u8]) -> Option<i64> {
    let (neg, digits) = if s.first() == Some(&b'-') {
        (true, &s[1..])
    } else {
        (false, s)
    };
    if digits.is_empty() {
        return None;
    }
    let mut n = 0i64;
    for &b in digits {
        if !b.is_ascii_digit() {
            return None;
        }
        n = n.checked_mul(10)?.checked_add((b - b'0') as i64)?;
    }
    Some(if neg { -n } else { n })
}

/// Lossily convert a byte slice to `&str` for use in `print!`.
/// Falls back to `"<non-utf8>"` if the slice is not valid UTF-8.
fn as_str(b: &[u8]) -> &str {
    core::str::from_utf8(b).unwrap_or("<non-utf8>")
}
