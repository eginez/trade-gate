//! trade-gate-proxy: policy-gated reverse proxy for scheduled purchases.
//!
//! Usage (env):
//!   PROXY_LISTEN=127.0.0.1:8080  UPSTREAM_HOST=api.schwabapi.com:443
//!   UPSTREAM_TLS=true             POLICIES_PATH=config/generated/policies.json
//!   ALLOWLIST_PATH=config/generated/allowlist.json
//!   DB_PATH=buys.db               CREDENTIAL_STORE=stub
//!
//! The proxy is NOT a daemon: it runs for the duration of one run
//! and exits when its parent (Docker Compose / the scheduler) stops
//! it. Nothing is left running between runs.

mod config;
mod credentials;
mod db;
mod gate;
mod policy;

use config::{Allowlist, PolicySet};
use credentials::credential_store_from_env;
use db::Db;
use gate::{Gate, GateState, PostClient, Upstream};
use hyper_util::client::legacy::Client;
use hyper_util::rt::TokioExecutor;
use pingora::prelude::*;
use std::collections::HashMap;
use std::sync::{Arc, Mutex};

fn env_or(key: &str, default: &str) -> String {
    std::env::var(key).unwrap_or_else(|_| default.to_string())
}

fn main() {
    let listen = env_or("PROXY_LISTEN", "127.0.0.1:8080");
    let upstream_host = env_or("UPSTREAM_HOST", "api.schwabapi.com:443");
    let upstream_tls = env_or("UPSTREAM_TLS", "true") != "false";
    let policies_path = env_or("POLICIES_PATH", "config/generated/policies.json");
    let allowlist_path = env_or("ALLOWLIST_PATH", "config/generated/allowlist.json");
    let db_path = env_or("DB_PATH", "buys.db");

    // Fail closed at startup: a bad policy / allowlist refuses to
    // start the proxy, instead of starting with a permissive one.
    let policies = std::fs::read_to_string(&policies_path)
        .unwrap_or_else(|e| panic!("cannot read {policies_path}: {e}"));
    let policy = PolicySet::load_from_str(&policies)
        .unwrap_or_else(|e| panic!("invalid policy: {e}"));
    let allowlist_json = std::fs::read_to_string(&allowlist_path)
        .unwrap_or_else(|e| panic!("cannot read {allowlist_path}: {e}"));
    let allowlist = Allowlist::load_from_str(&allowlist_json)
        .unwrap_or_else(|e| panic!("invalid allowlist: {e}"));
    eprintln!(
        "trade-gate: loaded {} permits, symbols {:?}, cap ${}, window {}d",
        policy.permits.len(),
        allowlist.symbols,
        allowlist.max_dollars_per_run,
        allowlist.rolling_window_days
    );

    let db = Arc::new(Db::open(&db_path).unwrap_or_else(|e| panic!("cannot open DB {db_path}: {e}")));
    let credentials =
        credential_store_from_env().unwrap_or_else(|e| panic!("credential store: {e}"));

    // Hyper client for store-and-forward POSTs. https_or_http so the
    // plain-HTTP mock upstream works in tests; real TLS uses the
    // system roots.
    let https = hyper_rustls::HttpsConnectorBuilder::new()
        .with_native_roots()
        .expect("cannot load native TLS roots")
        .https_or_http()
        .enable_http1()
        .build();
    let post_client: PostClient = Client::builder(TokioExecutor::new()).build(https);

    let state = Arc::new(GateState {
        policy,
        allowlist,
        db,
        credentials,
        upstream: Upstream { host: upstream_host, tls: upstream_tls },
        quote_cache: Mutex::new(HashMap::new()),
        pending_buys: Mutex::new(HashMap::new()),
        post_client,
    });

    let mut server = Server::new(None).expect("cannot create Pingora server");
    server.bootstrap();
    let mut proxy = http_proxy_service(&server.configuration, Gate::new(state));
    proxy.add_tcp(&listen);
    eprintln!("trade-gate: listening on {listen}");
    server.add_service(proxy);
    server.run_forever();
}
