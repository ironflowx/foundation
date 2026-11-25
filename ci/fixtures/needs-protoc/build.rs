//! Fails unless it can run both, found on PATH or named by the variable some build scripts read.

use std::process::Command;

fn main() {
    let mut missing = Vec::new();
    for (tool, variable) in [("protoc", "PROTOC"), ("buf", "BUF")] {
        let named = std::env::var(variable).unwrap_or_else(|_| tool.to_owned());
        if Command::new(named).status().is_err() {
            missing.push(format!("{tool} did not run\n"));
        }
    }
    assert!(missing.is_empty(), "\n{}", missing.concat());
}
