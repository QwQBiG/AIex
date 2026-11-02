#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]
#![forbid(unsafe_code)]

fn main() {
    let arguments: Vec<_> = std::env::args().skip(1).collect();
    #[cfg(windows)]
    let result = ai_ex_platform_windows::native::run_helper(&arguments);
    #[cfg(not(windows))]
    let result: std::io::Result<()> = {
        let _ = arguments;
        Err(std::io::Error::new(
            std::io::ErrorKind::Unsupported,
            "requires Windows",
        ))
    };
    if let Err(error) = result {
        eprintln!("{error}");
        std::process::exit(1);
    }
}
