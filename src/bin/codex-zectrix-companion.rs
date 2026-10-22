#![cfg_attr(windows, windows_subsystem = "windows")]

fn main() -> std::process::ExitCode {
    if std::env::args().nth(1).as_deref() == Some("version") {
        println!("{}", env!("CARGO_PKG_VERSION"));
        return std::process::ExitCode::SUCCESS;
    }
    codex_zectrix_dashboard::companion::run_companion()
}
