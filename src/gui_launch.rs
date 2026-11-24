//! Start the desktop in its own session and confirm actual window creation.
//! The startup pipe is private to the parent/child pair; no temporary files or
//! shell commands are involved, and the window never inherits terminal I/O.
//!
//! Child stdout is a one-shot protocol pipe, not a logging stream. The parent
//! closes it after receiving readiness. STARTUP_REPORTED prevents any later
//! protocol writes; desktop diagnostics must never use println! on this path.
//! In foreground test mode stdout/stderr remain ordinary inherited streams.
use std::{
    error::Error,
    io::{self, BufRead, BufReader, Read, Write},
    path::Path,
    process::{Command, Stdio},
    sync::{
        atomic::{AtomicBool, Ordering},
        mpsc,
    },
    thread,
    time::Duration,
};

const CHILD_ENV: &str = "PHOTOPINFIX_GUI_CHILD";
const FOREGROUND_ENV: &str = "PHOTOPINFIX_GUI_FOREGROUND";
const STARTUP_TIMEOUT: Duration = Duration::from_secs(30);
const MAX_MESSAGE_BYTES: u64 = 64 * 1024;
static STARTUP_REPORTED: AtomicBool = AtomicBool::new(false);

pub fn runs_in_foreground() -> bool {
    is_child() || std::env::var_os(FOREGROUND_ENV).is_some_and(|value| value == "1")
}

fn is_child() -> bool {
    std::env::var_os(CHILD_ENV).is_some_and(|value| value == "1")
}

/// Called by the native AppCreator after its window and application exist.
/// Foreground runs do not write startup messages into ordinary program output.
pub fn ready() -> io::Result<()> {
    if is_child() && !STARTUP_REPORTED.swap(true, Ordering::Relaxed) {
        write_message(serde_json::json!({ "photopinfix_gui": 1, "ready": true }))?;
    }
    Ok(())
}

/// Deliver initialization failures to the waiting terminal launcher. Errors
/// after a window has opened are outside the startup handshake.
pub fn report_error(error: &dyn Error) {
    if is_child() && !STARTUP_REPORTED.swap(true, Ordering::Relaxed) {
        let _ = write_message(serde_json::json!({
            "photopinfix_gui": 1,
            "error": error.to_string(),
        }));
    }
}

fn write_message(message: serde_json::Value) -> io::Result<()> {
    let mut output = io::stdout().lock();
    serde_json::to_writer(&mut output, &message)?;
    output.write_all(b"\n")?;
    output.flush()
}

pub fn launch(directory: Option<&Path>) -> io::Result<u32> {
    let executable = std::env::current_exe()?;
    let mut command = child_command(&executable, directory);
    detach(&mut command);
    let mut child = command
        .spawn()
        .map_err(|error| io::Error::new(error.kind(), format!("无法启动独立 GUI 进程：{error}")))?;
    let process = child.id();
    let output = child.stdout.take().expect("GUI startup pipe was requested");
    let (sender, receiver) = mpsc::sync_channel(1);
    let reader = thread::spawn(move || {
        let _ = sender.send(read_message(output));
    });
    let result = match receiver.recv_timeout(STARTUP_TIMEOUT) {
        Ok(result) => result,
        Err(mpsc::RecvTimeoutError::Timeout) => Err(io::Error::new(
            io::ErrorKind::TimedOut,
            "GUI 未在 30 秒内完成窗口初始化，已终止该启动进程。请检查图形桌面会话。",
        )),
        Err(mpsc::RecvTimeoutError::Disconnected) => {
            Err(io::Error::other("无法接收 GUI 窗口的启动结果"))
        }
    };
    if result.is_err() {
        // Failed/timed-out startup must not leave an unexpected window behind.
        let _ = child.kill();
        let _ = child.wait();
    }
    let _ = reader.join();
    result.map(|()| process)
}

fn child_command(executable: &Path, directory: Option<&Path>) -> Command {
    let mut command = Command::new(executable);
    command
        .arg("--gui")
        .env(CHILD_ENV, "1")
        .env_remove(FOREGROUND_ENV)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null());
    if let Some(directory) = directory {
        command.arg("--").arg(directory);
    }
    command
}

#[cfg(unix)]
fn detach(command: &mut Command) {
    use std::os::unix::process::CommandExt;
    unsafe extern "C" {
        fn setsid() -> std::os::raw::c_int;
    }
    // CommandExt::setsid is still unstable. setsid(2) is async-signal-safe;
    // this pre-exec callback does no allocation, locking or other library work
    // before a successful call. Failure is returned through std's exec pipe.
    unsafe {
        command.pre_exec(|| {
            if setsid() == -1 {
                Err(io::Error::last_os_error())
            } else {
                Ok(())
            }
        });
    }
}

#[cfg(windows)]
fn detach(command: &mut Command) {
    use std::os::windows::process::CommandExt;
    const DETACHED_PROCESS: u32 = 0x0000_0008;
    const CREATE_NEW_PROCESS_GROUP: u32 = 0x0000_0200;
    command.creation_flags(DETACHED_PROCESS | CREATE_NEW_PROCESS_GROUP);
}

#[cfg(not(any(unix, windows)))]
fn detach(_command: &mut Command) {}

fn read_message(output: impl Read) -> io::Result<()> {
    let mut line = String::new();
    BufReader::new(output)
        .take(MAX_MESSAGE_BYTES)
        .read_line(&mut line)?;
    if !line.ends_with('\n') {
        return Err(io::Error::other(
            "GUI 进程在确认窗口创建之前退出，或没有返回完整的启动结果",
        ));
    }
    let message: serde_json::Value = serde_json::from_str(&line)
        .map_err(|_| io::Error::other("GUI 进程返回了无法识别的启动结果"))?;
    if message
        .get("photopinfix_gui")
        .and_then(|value| value.as_u64())
        != Some(1)
    {
        return Err(io::Error::other("GUI 启动结果的协议版本不匹配"));
    }
    if let Some(error) = message.get("error").and_then(|value| value.as_str()) {
        return Err(io::Error::other(format!("无法打开 GUI 窗口：{error}")));
    }
    if message.get("ready").and_then(|value| value.as_bool()) != Some(true) {
        return Err(io::Error::other("GUI 进程未确认窗口创建成功"));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::ffi::OsStr;

    #[test]
    fn startup_requires_complete_ready_message_and_preserves_error_details() {
        assert!(read_message(b"{\"photopinfix_gui\":1,\"ready\":true}\n".as_slice()).is_ok());
        for invalid in [
            "",
            "{\"photopinfix_gui\":1,\"ready\":true}",
            "{\"photopinfix_gui\":2,\"ready\":true}\n",
            "{\"photopinfix_gui\":1,\"ready\":false}\n",
            "not a startup message\n",
        ] {
            assert!(read_message(invalid.as_bytes()).is_err());
        }
        let error = read_message(
            "{\"photopinfix_gui\":1,\"error\":\"缺少图形会话\\n详细原因\"}\n".as_bytes(),
        )
        .unwrap_err();
        assert!(error.to_string().contains("缺少图形会话\n详细原因"));
    }

    #[test]
    fn child_command_preserves_optional_unicode_and_option_like_paths() {
        let command = child_command(Path::new("photopinfix"), None);
        assert_eq!(
            command.get_args().collect::<Vec<_>>(),
            [OsStr::new("--gui")]
        );
        let command = child_command(Path::new("photopinfix"), Some(Path::new("-中文 照片")));
        assert_eq!(
            command.get_args().collect::<Vec<_>>(),
            [
                OsStr::new("--gui"),
                OsStr::new("--"),
                OsStr::new("-中文 照片")
            ]
        );
        assert!(
            command
                .get_envs()
                .any(|(name, value)| { name == CHILD_ENV && value == Some(OsStr::new("1")) })
        );
        assert!(
            command
                .get_envs()
                .any(|(name, value)| { name == FOREGROUND_ENV && value.is_none() })
        );
    }
}
