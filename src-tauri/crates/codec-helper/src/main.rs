#![forbid(unsafe_code)]

use std::io;
use std::process::ExitCode;

use imgviewer_codec_helper::{serve, validate_cli_arguments};

fn main() -> ExitCode {
    if validate_cli_arguments(std::env::args_os()).is_err() {
        return ExitCode::from(64);
    }

    let stdin = io::stdin();
    let stdout = io::stdout();
    if serve(&mut stdin.lock(), &mut stdout.lock()).is_err() {
        return ExitCode::from(70);
    }
    ExitCode::SUCCESS
}
