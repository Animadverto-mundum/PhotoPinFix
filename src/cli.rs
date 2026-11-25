use std::{ffi::OsString, io, path::PathBuf};

#[derive(Clone, Copy, Debug)]
pub enum Graphics {
    Auto,
    Halfblocks,
    Sixel,
    Kitty,
    Iterm2,
}

#[derive(Debug)]
pub enum Action {
    Open {
        directory: Option<PathBuf>,
        graphics: Graphics,
        gui: bool,
    },
    Help,
    Version,
}

pub fn parse(arguments: impl IntoIterator<Item = OsString>) -> io::Result<Action> {
    let mut arguments = arguments.into_iter();
    let mut directory = None;
    let mut options = true;
    let mut graphics = Graphics::Auto;
    let mut gui = false;
    while let Some(argument) = arguments.next() {
        if options {
            match argument.to_str() {
                Some("--") => {
                    options = false;
                    continue;
                }
                Some("--help" | "-h") => return Ok(Action::Help),
                Some("--version" | "-V") => return Ok(Action::Version),
                Some("--gui") => {
                    gui = true;
                    continue;
                }
                Some("--graphics") => {
                    graphics = match arguments.next().as_deref().and_then(|s| s.to_str()) {
                        Some("auto") => Graphics::Auto,
                        Some("halfblocks") => Graphics::Halfblocks,
                        Some("sixel") => Graphics::Sixel,
                        Some("kitty") => Graphics::Kitty,
                        Some("iterm2") => Graphics::Iterm2,
                        _ => {
                            return Err(io::Error::other(
                                "--graphics: auto / halfblocks / sixel / kitty / iterm2",
                            ));
                        }
                    };
                    continue;
                }
                _ if argument.to_string_lossy().starts_with('-') => {
                    return Err(io::Error::other(format!(
                        "未知选项：{}；使用 --help 查看用法",
                        argument.to_string_lossy()
                    )));
                }
                _ => {}
            }
        }
        if directory.replace(PathBuf::from(argument)).is_some() {
            return Err(io::Error::other("只能指定一个照片目录"));
        }
    }
    Ok(Action::Open {
        directory,
        graphics,
        gui,
    })
}

pub fn print_help() {
    println!(
        "PhotoPinFix {} — 照片 GPS 补全\n\n\
用法：photopinfix [--gui] [--graphics auto|halfblocks|sixel|kitty|iterm2] [目录]\n\
默认打开终端 TUI；--gui 打开独立桌面窗口，启动后可关闭终端。\n\
GUI 不指定目录时显示欢迎页，可在窗口中选择照片目录；需要图形桌面会话。\n\
GUI 支持鼠标操作，Ctrl+L 选择目录，Ctrl+Q 退出；--graphics 仅用于 TUI。\n\
TUI 不指定目录时处理当前目录；不递归扫描。RAW 与 JPEG/PNG/HEIF 同等处理。\n\n\
TUI 快捷键：\n\
↑/↓ 或 j/k 选择文件   ←/→ 切换拍摄 session   a 当前 session/全部\n\
Enter 确认写当前照片   Shift+Enter 或 W 确认写当前范围全部候选\n\
e 编辑坐标   p 前图   n 后图   c 最近   i 按时间插值   u 撤销手动候选\n\
s 临时设置   m 地图底图开关   b 切换地图源   +/- 缩放   f 全部点入镜\n\
H/J/K/L 平移地图   l 打开目录   Backspace 上级   r 重扫   ? 帮助   q 退出\n\n\
--help/-h 查看用法；--version/-V 查看版本；-- 后可传以 - 开头的目录。\n\
自动规则默认：相邻间隔 <60 秒，参考总时差 ≤600 秒；>1 小时分 session。\n\
GPS 和 PhotoPinFix:Mode/Method 仅在确认后写入；每张成功后默认删除临时备份，设置可保留。\n\
内置元数据引擎和 HEIC 解码，无需安装 Python、Perl 或 ExifTool。\n\
地图默认无底图；图片、地图与瓦片缓存仅在内存中处理。",
        env!("CARGO_PKG_VERSION")
    );
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::Path;

    fn args(arguments: &[&str]) -> io::Result<Action> {
        parse(arguments.iter().map(|argument| OsString::from(*argument)))
    }

    #[test]
    fn omitted_directory_is_distinct_from_an_explicit_current_directory() {
        assert!(matches!(
            args(&[]).unwrap(),
            Action::Open {
                directory: None,
                gui: false,
                graphics: Graphics::Auto
            }
        ));
        assert!(matches!(
            args(&["."]).unwrap(),
            Action::Open { directory: Some(directory), gui: false, .. }
                if directory == Path::new(".")
        ));
    }

    #[test]
    fn gui_flag_works_before_and_after_directory() {
        for arguments in [["--gui", "中文 照片"], ["中文 照片", "--gui"]] {
            assert!(matches!(
                args(&arguments).unwrap(),
                Action::Open { directory: Some(directory), gui: true, .. }
                    if directory == Path::new("中文 照片")
            ));
        }
        assert!(matches!(
            args(&["--gui"]).unwrap(),
            Action::Open {
                directory: None,
                gui: true,
                ..
            }
        ));
    }

    #[test]
    fn double_dash_stops_option_parsing() {
        assert!(matches!(
            args(&["--", "--gui"]).unwrap(),
            Action::Open { directory: Some(directory), gui: false, .. }
                if directory == Path::new("--gui")
        ));
        assert!(matches!(
            args(&["--gui", "--", "-照片"]).unwrap(),
            Action::Open { directory: Some(directory), gui: true, .. }
                if directory == Path::new("-照片")
        ));
    }

    #[test]
    fn invalid_options_and_multiple_paths_are_rejected() {
        for arguments in [
            vec!["--unknown"],
            vec!["--graphics"],
            vec!["--graphics", "unknown"],
            vec!["--gui", "one", "two"],
            vec!["one", "--gui", "two"],
            vec!["--", "one", "two"],
            vec!["one", "--", "--gui"],
        ] {
            assert!(args(&arguments).is_err(), "accepted {arguments:?}");
        }
    }

    #[test]
    fn gui_does_not_override_help_or_version() {
        assert!(matches!(args(&["--gui", "--help"]).unwrap(), Action::Help));
        assert!(matches!(
            args(&["--gui", "--version"]).unwrap(),
            Action::Version
        ));
    }
}
