//! `ironflowx`: the settings of `serve`, its log and its exit code.

use std::net::SocketAddr;
use std::path::PathBuf;
use std::process::ExitCode;

use clap::{Arg, Command, value_parser};

fn main() -> ExitCode {
    let matches = Command::new("ironflowx")
        .subcommand_required(true)
        .arg_required_else_help(true)
        .subcommand(
            Command::new("serve")
                .about("Opens the engine in a data directory and serves it")
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
        )
        .get_matches();
    let serve = matches
        .subcommand_matches("serve")
        .expect("the one command");
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
