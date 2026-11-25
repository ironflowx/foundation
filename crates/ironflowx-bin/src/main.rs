//! `ironflowx`: the settings of `serve`, its log and its exit code, and the CLI.

use std::net::SocketAddr;
use std::path::PathBuf;
use std::process::ExitCode;

use clap::{Arg, ArgMatches, Command, value_parser};

fn main() -> ExitCode {
    let root = Command::new("ironflowx").subcommand(
        Command::new("serve")
            .about("Open the engine in a data directory and serve it")
            .arg(
                Arg::new("data-dir")
                    .long("data-dir")
                    .value_name("PATH")
                    .help("The data directory, created if missing")
                    .value_parser(value_parser!(PathBuf))
                    .default_value("./.ironflowx"),
            )
            .arg(
                Arg::new("listen")
                    .long("listen")
                    .value_name("ADDRESS")
                    .help("The address to listen on; port 0 picks one")
                    .value_parser(value_parser!(SocketAddr))
                    .default_value("127.0.0.1:9310"),
            ),
    );
    let matches = match ironflowx_cli::parse(root) {
        Ok(matches) => matches,
        Err(code) => return code,
    };
    match matches.subcommand_matches("serve") {
        Some(serve) => run_serve(serve),
        None => ironflowx_cli::run(&matches),
    }
}

fn run_serve(serve: &ArgMatches) -> ExitCode {
    let data_dir = serve.get_one::<PathBuf>("data-dir").expect("has a default");
    let listen = serve
        .get_one::<SocketAddr>("listen")
        .expect("has a default");
    tracing_subscriber::fmt()
        .json()
        .with_writer(std::io::stderr)
        .with_max_level(tracing::Level::INFO)
        .init();
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
        .expect("a runtime starts");
    let Err(refused) = runtime.block_on(ironflowx_bin::serve(data_dir, *listen));
    tracing::error!("{refused}");
    ExitCode::from(refused.exit_code())
}
