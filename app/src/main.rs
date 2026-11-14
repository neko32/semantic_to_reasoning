//! `s2r` entry point.

use std::process::ExitCode;

use clap::Parser;
use s2r::api::cli::{Cli, run};

fn main() -> ExitCode {
    // Optional .env in the working directory; real environment variables win.
    let _ = dotenvy::dotenv();
    let cli = Cli::parse();
    match run(&cli) {
        Ok(out) => {
            println!("{out}");
            ExitCode::SUCCESS
        }
        Err(err) => {
            eprintln!("error: {err:#}");
            ExitCode::FAILURE
        }
    }
}
