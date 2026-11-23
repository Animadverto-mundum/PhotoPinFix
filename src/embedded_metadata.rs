//! In-process adapter for exiftool-rs 1.0.0. Its Rust interpreter executes the
//! embedded ExifTool modules; no Perl executable or metadata subprocess runs.
//! Keep the CLI's JSON, binary output and custom-schema behavior unchanged.

use std::{
    cell::RefCell,
    ffi::{OsStr, OsString},
    fmt, io,
    panic::{AssertUnwindSafe, catch_unwind},
    rc::Rc,
    sync::Mutex,
};

use exiftool_rs::{
    interp::{Binding, Flow, Want},
    perl::Value,
};

// Different MetadataTool instances can be used by parallel tests or consumers.
// Serialize the interpreter's process-facing runtime as well as each worker's
// existing I/O gate. Interp itself contains Rc and stays on the calling thread.
static ENGINE_GATE: Mutex<()> = Mutex::new(());

#[derive(Debug)]
pub(crate) struct Output {
    pub status: ExitStatus,
    pub stdout: Vec<u8>,
    pub stderr: Vec<u8>,
}

#[derive(Debug, Clone, Copy)]
pub(crate) struct ExitStatus(i32);

impl ExitStatus {
    pub fn success(self) -> bool {
        self.0 == 0
    }
}

impl fmt::Display for ExitStatus {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "{}", self.0)
    }
}

#[derive(Debug, Default)]
pub(crate) struct Command {
    args: Vec<OsString>,
}

impl Command {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn arg(&mut self, arg: impl AsRef<OsStr>) -> &mut Self {
        self.args.push(arg.as_ref().to_owned());
        self
    }

    pub fn args<I, S>(&mut self, args: I) -> &mut Self
    where
        I: IntoIterator<Item = S>,
        S: AsRef<OsStr>,
    {
        self.args
            .extend(args.into_iter().map(|arg| arg.as_ref().to_owned()));
        self
    }

    pub fn output(&mut self) -> io::Result<Output> {
        let _guard = ENGINE_GATE
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        catch_unwind(AssertUnwindSafe(|| run(&self.args))).map_err(|panic| {
            let message = panic
                .downcast_ref::<String>()
                .map(String::as_str)
                .or_else(|| panic.downcast_ref::<&str>().copied())
                .unwrap_or("unknown interpreter failure");
            io::Error::other(format!(
                "内置元数据引擎失败：{}",
                message.chars().take(800).collect::<String>()
            ))
        })
    }
}

fn argument_value(arg: &OsStr) -> Value {
    #[cfg(unix)]
    {
        use std::os::unix::ffi::OsStrExt;
        Value::bytes(arg.as_bytes())
    }
    #[cfg(not(unix))]
    {
        Value::bytes(arg.to_string_lossy().as_bytes())
    }
}

fn run(args: &[OsString]) -> Output {
    let mut interpreter = exiftool_rs::runtime::new_interp();
    // STDOUT/STDERR stay in byte buffers, including -b preview extraction.
    // Never let the metadata engine print into the terminal's TUI stream.
    interpreter.passthrough = false;
    interpreter.strict = false;
    interpreter.set_scalar("main::0", Value::str("photopinfix-embedded"));
    let argv = args.iter().map(|arg| argument_value(arg)).collect();
    interpreter.set_global(
        b'@',
        "main::ARGV",
        Binding::Array(Rc::new(RefCell::new(argv))),
    );
    exiftool_rs::cli::register(&mut interpreter);
    match interpreter.call_named("main::<program>", vec![], Want::Void) {
        Ok(_) => {}
        Err(Flow::Exit(code)) => interpreter.exit_code = Some(code),
        Err(Flow::Die(error)) => {
            interpreter
                .stderr
                .extend_from_slice(error.to_text().as_bytes());
            interpreter.exit_code = Some(255);
        }
        Err(_) => {
            interpreter
                .stderr
                .extend_from_slice(b"Unexpected embedded interpreter control flow\n");
            interpreter.exit_code = Some(255);
        }
    }
    if let Err(error) = interpreter.run_end_blocks() {
        if let Flow::Die(message) = error {
            interpreter
                .stderr
                .extend_from_slice(message.to_text().as_bytes());
        }
        interpreter.exit_code = Some(255);
    }
    interpreter.flush_handles();
    Output {
        status: ExitStatus(interpreter.exit_code.unwrap_or(0)),
        stdout: std::mem::take(&mut interpreter.stdout),
        stderr: std::mem::take(&mut interpreter.stderr),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn embedded_errors_are_returned_without_terminating_the_host() {
        let directory = tempfile::tempdir().unwrap();
        let output = Command::new()
            .args(["-config", "", "-j"])
            .arg(directory.path().join("missing.png"))
            .output()
            .unwrap();
        assert!(!output.status.success());
        assert!(!output.stderr.is_empty());
        let version = Command::new()
            .args(["-config", "", "-ver"])
            .output()
            .unwrap();
        assert!(version.status.success());
        assert!(
            String::from_utf8_lossy(&version.stdout)
                .trim()
                .starts_with("13.")
        );
    }

    #[cfg(unix)]
    #[test]
    fn unix_argument_conversion_keeps_non_utf8_bytes() {
        use std::os::unix::ffi::OsStrExt;
        let input = b"/tmp/non-utf8-\xff.jpg";
        assert_eq!(
            argument_value(OsStr::from_bytes(input)).to_bytes().as_ref(),
            input
        );
    }
}
