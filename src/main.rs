mod app;
mod cli;
mod gui;
mod gui_directory;
mod gui_launch;
mod gui_map;
mod gui_state;
mod input;
mod ui;
mod workers;

use std::{
    error::Error,
    io::{self, IsTerminal},
    path::PathBuf,
    process::ExitCode,
};

fn main() -> ExitCode {
    match run() {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            gui_launch::report_error(error.as_ref());
            eprintln!("PhotoPinFix: {error}");
            ExitCode::FAILURE
        }
    }
}

fn run() -> Result<(), Box<dyn Error>> {
    match cli::parse(std::env::args_os().skip(1))? {
        cli::Action::Help => cli::print_help(),
        cli::Action::Version => println!("PhotoPinFix {}", env!("CARGO_PKG_VERSION")),
        cli::Action::Open {
            directory,
            graphics,
            gui,
        } => {
            let directory = directory.map(resolve_directory).transpose()?;
            if gui {
                if !gui_launch::runs_in_foreground() {
                    let process = gui_launch::launch(directory.as_deref())?;
                    println!("PhotoPinFix GUI 已启动（PID {process}）");
                    return Ok(());
                }
                let tool =
                    photopinfix::metadata::MetadataTool::discover().map_err(io::Error::other)?;
                return gui::run(directory, tool);
            }
            let directory = match directory {
                Some(directory) => directory,
                None => resolve_directory(PathBuf::from("."))?,
            };
            if !io::stdin().is_terminal() || !io::stdout().is_terminal() {
                return Err(
                    "TUI 需要交互式终端。请在终端中运行，或用 --gui 打开桌面窗口；查看用法可使用 --help。"
                        .into(),
                );
            }
            let tool = photopinfix::metadata::MetadataTool::discover().map_err(io::Error::other)?;
            ratatui::run(|terminal| app::run(terminal, directory, tool, graphics))?;
        }
    }
    Ok(())
}

fn resolve_directory(directory: PathBuf) -> io::Result<PathBuf> {
    let directory = if directory.is_absolute() {
        directory
    } else {
        std::env::current_dir()?.join(directory)
    };
    if !std::fs::metadata(&directory)?.is_dir() {
        return Err(io::Error::other(format!(
            "目标不是目录：{}",
            directory.display()
        )));
    }
    Ok(directory)
}
