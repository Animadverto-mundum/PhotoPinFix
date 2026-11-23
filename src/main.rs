mod app;
mod cli;
mod gui;
mod gui_map;
mod gui_state;
mod input;
mod ui;
mod workers;

use std::{
    error::Error,
    io::{self, IsTerminal},
    process::ExitCode,
};

fn main() -> ExitCode {
    match run() {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
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
            let directory = if directory.is_absolute() {
                directory
            } else {
                std::env::current_dir()?.join(directory)
            };
            if !std::fs::metadata(&directory)?.is_dir() {
                return Err(format!("目标不是目录：{}", directory.display()).into());
            }
            if gui {
                let tool =
                    photopinfix::metadata::MetadataTool::discover().map_err(io::Error::other)?;
                return gui::run(directory, tool);
            }
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
