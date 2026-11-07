//! The Pingora gate: `ProxyHttp` implementation, fail-closed.
//!
//! Order of checks in `request_filter`, fail-closed at every step:
//!   1. `/healthz` — answered locally, never forwarded.
//!   2. Route exists (method + path template) — else 403 `no_route`.
//!   3. Body parses and matches the permit's body rule — else
//!      400 `invalid_json` / 403 `body_mismatch`.
//!   4. Symbol allowlisted, dollar cap (quantity x last quote price)
//!      — else 403 `symbol_not_allowlisted` / `over_cap`.
//!   5. Rolling-window duplicate (place_order only) — else 409.
//!   6. Forward, with the real credential injected in
//!      `upstream_request_filter`. The script's own Authorization
//!      header, if any, is replaced, never forwarded.
//!
//! Every call the script initiates gets a result: success passed
//! through, or an explicit 403 / 409 / 400 / 502 with a JSON reason.
//! Nothing is silently dropped, no empty 200, no hang.

use crate::config::{Allowlist, PolicySet};
use crate::credentials::CredentialStore;
use crate::db::Db;
use crate::policy::{evaluate, Decision, DenyReason};
use async_trait::async_trait;
use bytes::Bytes;
use http_body_util::{BodyExt, Full};
use hyper_util::client::legacy::{Client, connect::HttpConnector};
use pingora::http::ResponseHeader;
use pingora::prelude::*;
use std::collections::HashMap;
use std::sync::{Arc, Mutex};

/// Hyper client used for store-and-forward POSTs (see gate docs).
pub type PostClient = Client<hyper_rustls::HttpsConnector<HttpConnector>, Full<Bytes>>;

/// Upstream (broker) address, from the --upstream-host startup argument. TLS by default; plain HTTP
/// only for the mock upstream in tests.
#[derive(Debug, Clone)]
pub struct Upstream {
    pub host: String,
    pub tls: bool,
}

/// Shared, trusted state. The script container never sees any of this.
pub struct GateState {
    pub policy: PolicySet,
    pub allowlist: Allowlist,
    pub db: Arc<Db>,
    pub credentials: Box<dyn CredentialStore>,
    pub upstream: Upstream,
    /// Last quote price per symbol, populated from upstream quote
    /// responses. The script must quote a symbol before it can buy
    /// it — that is what makes the dollar cap checkable.
    pub quote_cache: Mutex<HashMap<String, f64>>,
    /// Orders placed (201) but not yet confirmed by a GET
    /// /orders/{id}: order_id -> (account_hash, symbol).
    pub pending_buys: Mutex<HashMap<String, (String, String)>>,
    /// Hyper client for store-and-forward POSTs.
    pub post_client: PostClient,
}

pub struct Gate {
    pub state: Arc<GateState>,
}

/// Per-request context, carried from request_filter to logging.
pub struct Ctx {
    pub action: Option<String>,
    pub account_hash: Option<String>,
    pub symbol: Option<String>,
    pub is_place_order: bool,
    pub order_id: Option<String>,
    /// Accumulated quote response body, for caching the price.
    pub quote_body: Vec<u8>,
}

impl Gate {
    pub fn new(state: Arc<GateState>) -> Self {
        Self { state }
    }

    async fn respond_json(
        session: &mut Session,
        status: u16,
        body: &serde_json::Value,
    ) -> Result<bool> {
        let bytes = serde_json::to_vec(body).unwrap_or_default();
        let mut header = ResponseHeader::build(status, Some(bytes.len()))
            .map_err(|e| Error::because(ErrorType::InternalError, "header", e))?;
        header
            .insert_header("Content-Type", "application/json")
            .map_err(|e| Error::because(ErrorType::InternalError, "header", e))?;
        header
            .set_content_length(bytes.len())
            .map_err(|e| Error::because(ErrorType::InternalError, "header", e))?;
        session.write_response_header(Box::new(header), false).await?;
        session.write_response_body(Some(Bytes::from(bytes)), true).await?;
        Ok(true) // response already sent — proxy exits
    }

    fn deny_json(reason: &DenyReason) -> serde_json::Value {
        let status = match reason {
            DenyReason::InvalidJson => 400,
            _ => 403,
        };
        serde_json::json!({
            "error": reason.code(),
            "message": reason.message(),
            "status": status,
        })
    }

    /// Store-and-forward a POST that passed the policy.
    ///
    /// Pingora 0.9 consumes a body read in request_filter and never
    /// replays it upstream (retry buffering is enabled only after
    /// request_filter), so the only correct way to forward an
    /// inspected POST is to forward it ourselves. The request is
    /// small (< 64 KiB) and the volume is ~1/month, so buffering
    /// the whole request and response is fine.
    ///
    /// Writes the upstream response back downstream (status, headers
    /// that matter — Content-Type, Location — and body) and returns
    /// Ok(true): response already sent. On upstream failure, returns
    /// an explicit 502 with a JSON reason — never a silent drop.
    async fn forward_post(
        &self,
        session: &mut Session,
        ctx: &mut Ctx,
        path: &str,
        query: &str,
        body_bytes: &[u8],
    ) -> Result<bool> {
        let scheme = if self.state.upstream.tls { "https" } else { "http" };
        let uri = if query.is_empty() {
            format!("{scheme}://{}{path}", self.state.upstream.host)
        } else {
            format!("{scheme}://{}{path}?{query}", self.state.upstream.host)
        };
        let token = self
            .state
            .credentials
            .get_access_token()
            .map_err(|_| Error::new(ErrorType::InternalError))?;
        let req = hyper::Request::builder()
            .method("POST")
            .uri(&uri)
            .header("Host", self.state.upstream.host.split(':').next().unwrap_or(""))
            .header("Content-Type", "application/json")
            .header("Authorization", format!("Bearer {}", token.expose()))
            .header("Content-Length", body_bytes.len().to_string())
            .body(Full::new(Bytes::copy_from_slice(body_bytes)))
            .map_err(|e| Error::because(ErrorType::InternalError, "building upstream request", e))?;

        let resp = match self.state.post_client.request(req).await {
            Ok(r) => r,
            Err(e) => {
                if let (Some(h), Some(s)) = (&ctx.account_hash, &ctx.symbol) {
                    self.state.db.record_attempt(h, s, "upstream_error");
                }
                let body = serde_json::json!({
                    "error": "upstream_error",
                    "message": format!("upstream did not answer: {e}"),
                    "status": 502,
                });
                return Self::respond_json(session, 502, &body).await;
            }
        };
        let status = resp.status().as_u16();
        let location = resp
            .headers()
            .get("location")
            .and_then(|v| v.to_str().ok())
            .map(|s| s.to_string());
        let content_type = resp
            .headers()
            .get("content-type")
            .and_then(|v| v.to_str().ok())
            .unwrap_or("application/json")
            .to_string();
        if ctx.is_place_order && status == 201 {
            ctx.order_id = location
                .as_ref()
                .and_then(|l| l.rsplit('/').next().map(|s| s.to_string()));
        }
        let body = resp
            .into_body()
            .collect()
            .await
            .map(|c| c.to_bytes())
            .unwrap_or_default();

        let mut header = ResponseHeader::build(status, Some(body.len()))
            .map_err(|e| Error::because(ErrorType::InternalError, "header", e))?;
        header
            .insert_header("Content-Type", content_type.as_str())
            .map_err(|e| Error::because(ErrorType::InternalError, "header", e))?;
        if let Some(loc) = &location {
            header
                .insert_header("Location", loc.as_str())
                .map_err(|e| Error::because(ErrorType::InternalError, "header", e))?;
        }
        header
            .set_content_length(body.len())
            .map_err(|e| Error::because(ErrorType::InternalError, "header", e))?;
        session.write_response_header(Box::new(header), false).await?;
        session.write_response_body(Some(body), true).await?;
        Ok(true)
    }
}

fn parse_price_from_quote_body(body: &[u8], symbol: &str) -> Option<f64> {
    // Accept both the mock shape {"VOO": {"lastPrice": 500.0}} and the
    // broker shape {"VOO": {"quote": {"lastPrice": 500.0}}}.
    let v: serde_json::Value = serde_json::from_slice(body).ok()?;
    fn find_last_price(v: &serde_json::Value) -> Option<f64> {
        match v {
            serde_json::Value::Object(m) => {
                if let Some(p) = m.get("lastPrice").and_then(|x| x.as_f64()) {
                    return Some(p);
                }
                m.values().find_map(find_last_price)
            }
            _ => None,
        }
    }
    v.get(symbol).and_then(find_last_price)
}

#[async_trait]
impl ProxyHttp for Gate {
    type CTX = Ctx;

    fn new_ctx(&self) -> Self::CTX {
        Ctx {
            action: None,
            account_hash: None,
            symbol: None,
            is_place_order: false,
            order_id: None,
            quote_body: Vec::new(),
        }
    }

    async fn request_filter(&self, session: &mut Session, ctx: &mut Self::CTX) -> Result<bool> {
        let header = session.req_header();
        let method = header.method.as_str().to_string();
        let path = header.uri.path().to_string();
        let query = header.uri.query().unwrap_or("").to_string();

        // Local health check — never forwarded upstream.
        if method == "GET" && path == "/healthz" {
            return Self::respond_json(
                session,
                200,
                &serde_json::json!({"status": "ok"}),
            )
            .await;
        }

        // Read and buffer the whole body (small order JSON, < 64 KiB).
        // A single read_request_body() call returns one chunk only —
        // loop until None so chunked / multi-chunk bodies are whole.
        //
        // NOTE (Pingora 0.9): a body read here is consumed, not
        // replayed to upstream — retry buffering is only enabled
        // *after* request_filter, and there is no public API to
        // inject a body back into the downstream stream. So POSTs
        // are store-and-forwarded in forward_post() below with a
        // plain hyper client, instead of streaming through Pingora.
        // GETs (no body to inspect) stream through Pingora normally.
        let mut body_bytes: Vec<u8> = Vec::new();
        loop {
            match session.read_request_body().await {
                Ok(Some(b)) => {
                    body_bytes.extend_from_slice(&b);
                    if body_bytes.len() > 64 * 1024 {
                        let reason = DenyReason::BodyMismatch("body exceeds 64 KiB".into());
                        return Self::respond_json(session, 403, &Self::deny_json(&reason)).await;
                    }
                }
                Ok(None) => break,
                Err(_) => {
                    let reason = DenyReason::InvalidJson;
                    return Self::respond_json(session, 400, &Self::deny_json(&reason)).await;
                }
            }
        }

        // Price for the cap check, from the quote cache (script quotes
        // first — see the call sequence in the README).
        let price = {
            // Symbol is not known yet; try every cached symbol is
            // wrong — instead, the evaluator needs the price for the
            // symbol in the body. Parse the symbol cheaply here.
            let sym: Option<String> = serde_json::from_slice::<serde_json::Value>(&body_bytes)
                .ok()
                .and_then(|v| {
                    v.get("orderLegCollection")?
                        .as_array()?
                        .first()?
                        .get("instrument")?
                        .get("symbol")?
                        .as_str()
                        .map(|s| s.to_string())
                });
            sym.and_then(|s| self.state.quote_cache.lock().unwrap().get(&s).copied())
        };

        let (decision, order) = evaluate(
            &self.state.policy,
            &self.state.allowlist,
            &method,
            &path,
            &query,
            &body_bytes,
            price,
        );

        match decision {
            Decision::Deny(reason) => {
                let status = match reason {
                    DenyReason::InvalidJson => 400,
                    _ => 403,
                };
                if let Some(o) = &order {
                    self.state.db.record_attempt(
                        o.path_params.first().map(|(_, v)| v.as_str()).unwrap_or(""),
                        &o.symbol,
                        "denied",
                    );
                }
                return Self::respond_json(session, status, &Self::deny_json(&reason)).await;
            }
            Decision::Allow { action } => {
                ctx.action = Some(action.clone());
                ctx.is_place_order = action == "place_order";
                if let Some(o) = order {
                    ctx.account_hash = o
                        .path_params
                        .iter()
                        .find(|(k, _)| k == "hash")
                        .map(|(_, v)| v.clone());
                    ctx.symbol = Some(o.symbol.clone());
                    // Rolling-window duplicate check (place_order only).
                    if ctx.is_place_order {
                        if let Some(hash) = &ctx.account_hash {
                            if self.state.db.duplicate_in_window(
                                hash,
                                &o.symbol,
                                self.state.allowlist.rolling_window_days,
                            ) {
                                let body = serde_json::json!({
                                    "error": "duplicate_buy_in_window",
                                    "message": format!(
                                        "{} was already bought for this account inside the {}-day window",
                                        o.symbol, self.state.allowlist.rolling_window_days
                                    ),
                                    "status": 409,
                                });
                                return Self::respond_json(session, 409, &body).await;
                            }
                        }
                    }
                }
                // Remember the account hash even for GETs (from the path).
                if ctx.account_hash.is_none() {
                    ctx.account_hash = path
                        .split('/')
                        .nth(4)
                        .filter(|s| !s.is_empty())
                        .map(|s| s.to_string());
                }
                // POSTs: store-and-forward in this filter (see note
                // above). GETs: continue to Pingora's upstream.
                if method == "POST" {
                    return self.forward_post(session, ctx, &path, &query, &body_bytes).await;
                }
                Ok(false) // continue to upstream
            }
        }
    }

    async fn upstream_peer(&self, _session: &mut Session, _ctx: &mut Self::CTX) -> Result<Box<HttpPeer>> {        let up = &self.state.upstream;
        let sni = if up.tls {
            up.host.split(':').next().unwrap_or("").to_string()
        } else {
            String::new()
        };
        Ok(Box::new(HttpPeer::new(&up.host, up.tls, sni)))
    }

    async fn upstream_request_filter(
        &self,
        _session: &mut Session,
        upstream_request: &mut RequestHeader,
        _ctx: &mut Self::CTX,
    ) -> Result<()> {
        // Inject the real credential; replace any Authorization the
        // script sent — it is never forwarded.
        let token = self
            .state
            .credentials
            .get_access_token()
            .map_err(|e| Error::because(ErrorType::InternalError, "credential store", e))?;
        upstream_request
            .insert_header("Authorization", format!("Bearer {}", token.expose()))
            .map_err(|e| Error::because(ErrorType::InternalError, "header", e))?;
        Ok(())
    }

    async fn upstream_response_filter(
        &self,
        _session: &mut Session,
        upstream_response: &mut ResponseHeader,
        ctx: &mut Self::CTX,
    ) -> Result<()> {
        // Capture the order id of a 201 place_order from Location.
        if ctx.is_place_order && upstream_response.status.as_u16() == 201 {
            if let Some(loc) = upstream_response.headers.get("location") {
                if let Ok(loc) = loc.to_str() {
                    ctx.order_id = loc.rsplit('/').next().map(|s| s.to_string());
                }
            }
        }
        Ok(())
    }

    fn upstream_response_body_filter(
        &self,
        _session: &mut Session,
        body: &mut Option<Bytes>,
        _end_of_stream: bool,
        ctx: &mut Self::CTX,
    ) -> Result<Option<std::time::Duration>> {
        // Accumulate quote response bodies so the price can be cached
        // in logging(), after the full body has arrived.
        if ctx.action.as_deref() == Some("get_quote") {
            if let Some(b) = body {
                ctx.quote_body.extend_from_slice(b);
            }
        }
        Ok(None)
    }

    async fn logging(&self, session: &mut Session, e: Option<&Error>, ctx: &mut Self::CTX) {
        let status = session.response_written().map(|r| r.status.as_u16());
        let action = ctx.action.clone().unwrap_or_default();

        if let Some(err) = e {
            // Upstream timed out / never answered: record an attempt,
            // never a buy. The downstream already gets Pingora's 502.
            eprintln!("trade-gate: upstream error for {action}: {err}");
            if let (Some(h), Some(s)) = (&ctx.account_hash, &ctx.symbol) {
                self.state.db.record_attempt(h, s, "upstream_error");
            }
            return;
        }

        // Cache quote prices for the next buy's cap check.
        if action == "get_quote" && status == Some(200) && !ctx.quote_body.is_empty() {
            if let Ok(v) = serde_json::from_slice::<serde_json::Value>(&ctx.quote_body) {
                if let Some(obj) = v.as_object() {
                    let mut cache = self.state.quote_cache.lock().unwrap();
                    for (sym, _) in obj {
                        if let Some(p) = parse_price_from_quote_body(&ctx.quote_body, sym) {
                            cache.insert(sym.clone(), p);
                        }
                    }
                }
            }
        }

        // A 201 place_order is only *pending* until a GET /orders/{id}
        // confirms it — only then is a buy row written.
        if ctx.is_place_order && status == Some(201) {
            if let (Some(h), Some(s), Some(id)) = (&ctx.account_hash, &ctx.symbol, &ctx.order_id) {
                self.state
                    .pending_buys
                    .lock()
                    .unwrap()
                    .insert(id.clone(), (h.clone(), s.clone()));
            }
            return;
        }
        if ctx.is_place_order {
            if let (Some(h), Some(s)) = (&ctx.account_hash, &ctx.symbol) {
                self.state.db.record_attempt(h, s, "place_not_created");
            }
            return;
        }
        if action == "preview_order" {
            if let (Some(h), Some(s)) = (&ctx.account_hash, &ctx.symbol) {
                self.state.db.record_attempt(h, s, "preview");
            }
            return;
        }
        if action == "get_order" && status == Some(200) {
            // Confirmation: if this order id was pending, record the buy.
            if let Some(id) = &ctx.order_id.clone().or_else(|| {
                session
                    .req_header()
                    .uri
                    .path()
                    .rsplit('/')
                    .next()
                    .map(|s| s.to_string())
            }) {
                if let Some((h, s)) = self.state.pending_buys.lock().unwrap().remove(id) {
                    self.state.db.record_buy(&h, &s, id);
                }
            }
        }
    }
}
