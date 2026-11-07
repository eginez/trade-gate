//! Credential store interface — STUB ONLY.
//!
//! No real secrets anywhere in this repo. The proxy needs, at runtime:
//!   - the App Key + App Secret (static, read at startup),
//!   - the refresh token (must be writable back on rotation),
//!   - the access token (memory only, never written to disk).
//!
//! The intended backend is `docker-credential-pass` (GPG `pass`
//! store, via the `docker-credential-pass` helper binary): the proxy
//! shells out to `docker-credential-pass get` / `store` with the
//! standard `{ServerURL, Username, Secret}` JSON protocol. That
//! backend is NOT implemented yet — this file is the trait it will
//! implement, plus a stub that returns a clearly-marked fake token
//! and warns on every use, so nothing can accidentally trade with a
//! stub in production.
//!
//! Real Schwab OAuth (authorization_code + refresh_token, 7-day
//! refresh expiry, client_secret_basic) is also NOT implemented yet.

use std::sync::Mutex;

/// In-memory secret. Deliberately no `Display`/`Debug` that would
/// print the value; use `expose()` only at the point of use.
pub struct SecretString(String);

impl SecretString {
    pub fn new(s: String) -> Self {
        Self(s)
    }
    pub fn expose(&self) -> &str {
        &self.0
    }
}

pub trait CredentialStore: Send + Sync {
    fn get_access_token(&self) -> Result<SecretString, String>;
    // Not used by the proxy yet — refresh-token rotation is stubbed
    // until real Schwab OAuth lands. Kept in the trait so the
    // backend contract is visible.
    #[allow(dead_code)]
    fn get_refresh_token(&self) -> Result<SecretString, String>;
    #[allow(dead_code)]
    fn store_refresh_token(&self, token: &str) -> Result<(), String>;
}

/// Stub backend. Returns a fake token. Logs a warning every time so
/// a stub can never be mistaken for real credentials.
pub struct StubCredentialStore {
    access_token: Mutex<String>,
}

impl StubCredentialStore {
    pub fn new() -> Self {
        eprintln!("WARNING: using STUB credential store — no real Schwab credentials. Do not trade with this.");
        Self { access_token: Mutex::new("STUB-ACCESS-TOKEN-NOT-A-REAL-TOKEN".into()) }
    }
}

impl CredentialStore for StubCredentialStore {
    fn get_access_token(&self) -> Result<SecretString, String> {
        Ok(SecretString::new(self.access_token.lock().unwrap().clone()))
    }
    fn get_refresh_token(&self) -> Result<SecretString, String> {
        Ok(SecretString::new("STUB-REFRESH-TOKEN-NOT-A-REAL-TOKEN".into()))
    }
    fn store_refresh_token(&self, _token: &str) -> Result<(), String> {
        // Stub: rotation is a no-op. The real backend must persist it.
        Ok(())
    }
}

/// Backend selector. Only `stub` exists for now; any other value is
/// a startup error, not a silent fallback to the stub.
pub fn credential_store_from_env() -> Result<Box<dyn CredentialStore>, String> {
    match std::env::var("CREDENTIAL_STORE").as_deref() {
        Ok("stub") | Err(_) => Ok(Box::new(StubCredentialStore::new())),
        Ok(other) => Err(format!("unknown CREDENTIAL_STORE {other:?} (only 'stub' exists for now)")),
    }
}
