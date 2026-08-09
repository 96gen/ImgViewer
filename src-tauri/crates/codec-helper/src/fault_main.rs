#![forbid(unsafe_code)]

#[cfg(not(debug_assertions))]
compile_error!("imgviewer-codec-fault-helper is test-only and must not be built for production");

use std::io;
use std::process::ExitCode;

use imgviewer_codec_helper::{serve_fault, validate_cli_arguments};

fn main() -> ExitCode {
    if validate_cli_arguments(std::env::args_os()).is_err() {
        return ExitCode::from(64);
    }

    let stdin = io::stdin();
    let stdout = io::stdout();
    if serve_fault(&mut stdin.lock(), &mut stdout.lock()).is_err() {
        return ExitCode::from(70);
    }
    ExitCode::SUCCESS
}
