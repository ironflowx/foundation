//! Ask a served engine which key is calling.

use std::error::Error;

use ironflowx::core::Secret;
use ironflowx::proto::{Call, Client, Operations as _, WhoamiRequest};

fn main() -> Result<(), Box<dyn Error>> {
    let [url, key_file] = &std::env::args_os().skip(1).collect::<Vec<_>>()[..] else {
        return Err("usage: whoami <url> <key-file>".into());
    };

    let client = Client::new(url.to_str().ok_or("the URL must be UTF-8")?)?;
    let mut key = std::fs::read_to_string(key_file)?;
    key.truncate(key.trim_end_matches(['\r', '\n']).len());
    let call = Call::with_key(Secret::new(key));

    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()?;
    let identity = runtime
        .block_on(client.whoami(&call, WhoamiRequest {}))
        .map_err(|error| format!("whoami failed: {:?}", error.kind))?;
    println!("{} with scopes {:?}", identity.key_id, identity.scopes);
    Ok(())
}
