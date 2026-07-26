//! Admin "draw anywhere" account tool (author request): connects with a
//! fresh identity, claims admin via `claim_admin`, and prints the resulting
//! identity + reconnection token so the token can be pasted into the web
//! client's "Paste an ID" field to restore this same admin account
//! elsewhere (or on another machine, e.g. against the VPS). Kept as a
//! reusable ops tool, not deleted after use like the/ sessions'
//! throwaway `hexa_probe.rs` — see WORK.md's "Admin account" section.
//!
//! Usage: `cargo run -p client --bin admin_probe -- <password> [host]`
//! `host` defaults to the local dev instance; pass the VPS's SpacetimeDB URL
//! to claim admin there instead.

#[path = "../module_bindings/mod.rs"]
mod module_bindings;
use module_bindings::*;

use spacetimedb_sdk::{credentials, DbContext};
use std::sync::{Arc, Mutex};
use std::time::Duration;

const DB_NAME: &str = "hexel";

/// `HEXEL_DB` overrides the target database, matching the game client — an
/// admin token is only valid for the database it was minted against.
fn db_name() -> String {
    std::env::var("HEXEL_DB").unwrap_or_else(|_| DB_NAME.to_string())
}

fn main() {
    let password = std::env::args().nth(1).unwrap_or_else(|| {
        eprintln!("usage: admin_probe <password> [host]");
        std::process::exit(2);
    });
    let host = std::env::args().nth(2).unwrap_or_else(|| "http://localhost:3000".to_string());

    // Dedicated creds key: never collides with the human client's own
    // identity or the bots' — a brand-new identity is minted the first time
    // this runs (no file at this key yet), and reused on any later re-run.
    let creds_store = || credentials::File::new("hexel-admin");
    let token_holder: Arc<Mutex<Option<String>>> = Arc::new(Mutex::new(None));
    let token_holder2 = token_holder.clone();

    let ctx = DbConnection::builder()
        .on_connect(move |_ctx, _identity, token| {
            *token_holder2.lock().unwrap() = Some(token.to_string());
            if let Err(e) = creds_store().save(token) {
                eprintln!("Failed to save credentials: {e:?}");
            }
        })
        .on_connect_error(|_ctx, err| {
            eprintln!("Connection error: {err:?}");
            std::process::exit(1);
        })
        .on_disconnect(|_ctx, err| {
            if let Some(e) = err {
                eprintln!("Disconnected: {e:?}");
            }
        })
        .with_token(creds_store().load().expect("Error loading credentials"))
        .with_database_name(db_name())
        .with_uri(&host)
        .build()
        .expect("Failed to connect");

    ctx.run_threaded();

    while ctx.try_identity().is_none() {
        std::thread::sleep(Duration::from_millis(20));
    }
    let identity = ctx.try_identity().expect("identity available after connect");

    let (tx, rx) = std::sync::mpsc::sync_channel(1);
    ctx.reducers
        .claim_admin_then(password, move |_ctx, result| {
            let outcome = match result {
                Ok(Ok(())) => Ok(()),
                Ok(Err(message)) => Err(message),
                Err(error) => Err(format!("internal reducer error: {error}")),
            };
            let _ = tx.send(outcome);
        })
        .unwrap_or_else(|error| {
            eprintln!("Failed to send claim_admin: {error}");
            std::process::exit(1);
        });

    match rx.recv_timeout(Duration::from_secs(10)) {
        Ok(Ok(())) => {}
        Ok(Err(message)) => {
            eprintln!("claim_admin rejected: {message}");
            std::process::exit(1);
        }
        Err(error) => {
            eprintln!("Timed out waiting for claim_admin: {error}");
            std::process::exit(1);
        }
    }

    let token = token_holder.lock().unwrap().clone().expect("no token captured");
    println!("identity: {identity}");
    println!("token: {token}");
}
