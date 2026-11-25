//! `ironflowx`: the settings of `serve`, its log and its exit code, and the CLI.

mod settings;

use std::io::{self, Write as _};
use std::process::ExitCode;

use clap::{ArgMatches, Command};
use ironflowx_core::ErrorKind;
use ironflowx_proto::OpError;
use tracing_subscriber::{Layer as _, filter, prelude::*};

fn main() -> ExitCode {
    let root = Command::new("ironflowx")
        .subcommand(settings::command())
        .subcommand(
            Command::new("seed")
                .about("Make a seed for an engine")
                .subcommand_required(true)
                .subcommand(Command::new("generate").about("Print a new seed and its key ID")),
        );
    let matches = match ironflowx_cli::parse(root) {
        Ok(matches) => matches,
        Err(code) => return code,
    };
    match matches.subcommand() {
        Some(("serve", serve)) => ironflowx_cli::end(run_serve(serve)),
        Some(("seed", _)) => ironflowx_cli::end(generate_seed()),
        _ => ironflowx_cli::run(&matches),
    }
}

fn run_serve(args: &ArgMatches) -> Result<(), OpError> {
    let settings = settings::Settings::read(args)?;
    // Dependencies can trace request paths, headers and bodies. Only host and door events go out.
    let targets = filter::Targets::new()
        .with_target("ironflowx_bin", settings.log_level)
        .with_target("ironflowx_doors", settings.log_level);
    if settings.log_format == "json" {
        tracing_subscriber::registry()
            .with(
                tracing_subscriber::fmt::layer()
                    .json()
                    .with_writer(io::stderr)
                    .with_filter(targets),
            )
            .init();
    } else {
        tracing_subscriber::registry()
            .with(
                tracing_subscriber::fmt::layer()
                    .with_ansi(false)
                    .with_writer(io::stderr)
                    .with_filter(targets),
            )
            .init();
    }
    settings::warn_unread_variables();
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
        .map_err(|e| internal(format!("cannot start the host runtime: {e}")))?;
    let Err(refused) = runtime.block_on(ironflowx_bin::serve_with_seed(
        &settings.data_dir,
        settings.listen,
        settings.seed,
    ));
    let error = refused.envelope();
    tracing::error!(target: "ironflowx_bin", "{}", error.message);
    Err(error)
}

fn generate_seed() -> Result<(), OpError> {
    let (id, key) = ironflowx_operations::generate_seed();
    // The key format needs no JSON escaping. Write it straight to its intended destination.
    writeln!(
        io::stdout(),
        r#"{{"key":"{}","key_id":"{id}"}}"#,
        key.expose()
    )
    .map_err(|e| internal(format!("cannot write to stdout: {e}")))
}

fn internal(message: String) -> OpError {
    OpError {
        kind: ErrorKind::Internal,
        message,
        hint: None,
    }
}
