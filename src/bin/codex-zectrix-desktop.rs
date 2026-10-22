#![cfg_attr(windows, windows_subsystem = "windows")]
fn main() -> std::process::ExitCode {
    let mut args = std::env::args().skip(1);
    if let Some(command) = args.next() {
        if args.next().is_some() {
            return std::process::ExitCode::FAILURE;
        }
        match command.as_str() {
            "--version" | "version" => println!("{}", env!("CARGO_PKG_VERSION")),
            "build-fingerprint" => println!(
                "{}",
                option_env!("CODEX_ZECTRIX_SOURCE_FINGERPRINT").unwrap_or("development")
            ),
            _ => return std::process::ExitCode::FAILURE,
        }
        return std::process::ExitCode::SUCCESS;
    }
    if let Err(error) = codex_zectrix_dashboard::desktop::app::run_desktop() {
        #[cfg(windows)]
        {
            let text = error
                .to_string()
                .encode_utf16()
                .chain(Some(0))
                .collect::<Vec<_>>();
            let title = "NOTE4 控制台启动失败"
                .encode_utf16()
                .chain(Some(0))
                .collect::<Vec<_>>();
            unsafe {
                windows_sys::Win32::UI::WindowsAndMessaging::MessageBoxW(
                    std::ptr::null_mut(),
                    text.as_ptr(),
                    title.as_ptr(),
                    windows_sys::Win32::UI::WindowsAndMessaging::MB_ICONERROR,
                );
            }
        }
        #[cfg(not(windows))]
        eprintln!("{error}");
        return std::process::ExitCode::FAILURE;
    }
    std::process::ExitCode::SUCCESS
}
