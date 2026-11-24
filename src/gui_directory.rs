//! Native folder selection with a nonblocking result channel for the GUI.
//!
//! Call `request` on the GUI thread: macOS creates NSOpenPanel there. Waiting
//! happens on a worker, never in the render loop. Linux uses the desktop portal
//! directly and does not spawn a dialog helper or silently replace native UI.
#[cfg(any(target_os = "windows", target_os = "macos"))]
use std::future::Future;
use std::{
    path::PathBuf,
    sync::mpsc::{self, Receiver, TryRecvError},
};

pub type DirectoryResult = Result<Option<PathBuf>, String>;

pub struct NativeDirectoryDialog {
    receiver: Receiver<DirectoryResult>,
    finished: bool,
}

impl NativeDirectoryDialog {
    /// Starts the platform picker. Must be called on the GUI/main thread.
    /// The initial folder is a hint; cancellation never changes the app state.
    pub fn request(start: Option<PathBuf>) -> Self {
        let (sender, receiver) = mpsc::channel();
        // Construct the native future before moving it: NSOpenPanel is a
        // main-thread object even though awaiting its result is Send.
        let task = pick_directory(start);
        let failed = sender.clone();
        if let Err(error) = std::thread::Builder::new()
            .name("native-directory-dialog".into())
            .spawn(move || {
                let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                    pollster::block_on(task).and_then(validate_selection)
                }))
                .unwrap_or_else(|_| Err("系统文件夹选择器异常退出".into()));
                let _ = sender.send(result);
            })
        {
            let _ = failed.send(Err(format!("无法启动系统文件夹选择器：{error}")));
        }
        Self {
            receiver,
            finished: false,
        }
    }

    /// None: pending/already consumed. Some(Ok(None)): user cancelled.
    /// The result is delivered once; the UI can poll alongside other workers.
    pub fn poll(&mut self) -> Option<DirectoryResult> {
        if self.finished {
            return None;
        }
        match self.receiver.try_recv() {
            Ok(result) => {
                self.finished = true;
                Some(result)
            }
            Err(TryRecvError::Empty) => None,
            Err(TryRecvError::Disconnected) => {
                self.finished = true;
                Some(Err("系统文件夹选择器未返回结果".into()))
            }
        }
    }
}

fn validate_selection(selection: Option<PathBuf>) -> DirectoryResult {
    if let Some(path) = &selection {
        std::fs::read_dir(path).map_err(|error| format!("无法读取所选文件夹：{error}"))?;
    }
    Ok(selection)
}

#[cfg(any(target_os = "windows", target_os = "macos"))]
fn pick_directory(start: Option<PathBuf>) -> impl Future<Output = DirectoryResult> + Send {
    let mut dialog = rfd::AsyncFileDialog::new().set_title("PhotoPinFix — 选择照片文件夹");
    if let Some(path) = start {
        dialog = dialog.set_directory(path);
    }
    let pending = dialog.pick_folder();
    async move { Ok(pending.await.map(|folder| folder.path().to_path_buf())) }
}

#[cfg(target_os = "linux")]
async fn pick_directory(start: Option<PathBuf>) -> DirectoryResult {
    use ashpd::desktop::{ResponseError, file_chooser::OpenFileRequest};

    // The directory option was introduced in FileChooser v3. Older portals
    // silently ignore it and display a file picker, which cannot satisfy this
    // operation. Probe the actual service before opening any native window.
    let version = async {
        let connection = ashpd::zbus::Connection::session().await?;
        let proxy = ashpd::zbus::Proxy::new(
            &connection,
            "org.freedesktop.portal.Desktop",
            "/org/freedesktop/portal/desktop",
            "org.freedesktop.portal.FileChooser",
        )
        .await?;
        proxy.get_property::<u32>("version").await
    }
    .await
    .map_err(|error| {
        format!("无法连接系统文件夹选择服务：{error}。请在目录栏直接输入照片路径。")
    })?;
    if version < 3 {
        return Err("系统文件选择服务版本过旧，不支持选择文件夹（需要 FileChooser v3 或更新版本）。请在目录栏直接输入照片路径。".into());
    }
    let request = OpenFileRequest::default()
        .title("PhotoPinFix — 选择照片文件夹")
        .multiple(false)
        .directory(true)
        .modal(true)
        .current_folder::<&std::path::Path>(start.as_deref())
        .map_err(|error| format!("无法设置初始文件夹：{error}"))?
        .send()
        .await;
    let selected = match request.and_then(|request| request.response()) {
        Ok(selected) => selected,
        Err(ashpd::Error::Response(ResponseError::Cancelled)) => return Ok(None),
        Err(error) => {
            return Err(format!(
                "系统文件夹选择器不可用：{error}。请确认桌面的 XDG Desktop Portal 文件选择服务正常，或在目录栏直接输入路径。"
            ));
        }
    };
    let uri = selected
        .uris()
        .first()
        .ok_or("系统文件夹选择器没有返回文件夹")?;
    let path = uri
        .to_file_path()
        .map_err(|_| "系统选择器返回了非本地文件夹，当前只支持本地照片目录")?;
    Ok(Some(path))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pending_cancel_and_completion_are_distinct_and_delivered_once() {
        let (sender, receiver) = mpsc::channel();
        let mut dialog = NativeDirectoryDialog {
            receiver,
            finished: false,
        };
        assert!(dialog.poll().is_none());
        sender.send(Ok(None)).unwrap();
        assert_eq!(dialog.poll(), Some(Ok(None)));
        assert!(dialog.poll().is_none());
    }

    #[test]
    fn missing_worker_is_error_and_file_selection_is_rejected() {
        let (sender, receiver) = mpsc::channel();
        let mut dialog = NativeDirectoryDialog {
            receiver,
            finished: false,
        };
        drop(sender);
        assert!(dialog.poll().unwrap().is_err());
        assert!(dialog.poll().is_none());
        let file = tempfile::NamedTempFile::new().unwrap();
        assert!(validate_selection(Some(file.path().to_path_buf())).is_err());
        let folder = tempfile::tempdir().unwrap();
        assert_eq!(
            validate_selection(Some(folder.path().to_path_buf())).unwrap(),
            Some(folder.path().to_path_buf())
        );
    }
}
