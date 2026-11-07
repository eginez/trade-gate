//! Pkl policy evaluator, permits-only. NO Cedar.
//!
//! The policy is Pkl data rendered to JSON (`config/generated/`).
//! This module loads that JSON into the structs in `config.rs` and
//! evaluates a request against it directly. There is no
//! `cedar-policy` crate and no `.cedar` output anywhere in this repo.
//!
//! Deny-by-default, in two layers:
//!   1. The Pkl schema (closed action union, literal `"BUY"`, no
//!      wildcard / allow-all) — a bad policy is a Pkl compile error.
//!   2. Here: no permit match, or any evaluation error, is a deny.
//!      Never fail open.
//!
//! What this module does NOT do (deliberately, stays in Rust in the
//! gate): the rolling-window DB check, the live-price dollar cap
//! beyond the permit's own `maxDollars`, and token injection.

use crate::config::{Allowlist, PolicySet};
use serde_json::Value;

/// Why a request was denied. Serialized as the `error` field of the
/// 403 JSON body the proxy returns — the script always gets a reason.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DenyReason {
    /// No permit matches this method + path at all.
    NoRoute,
    /// The body is not valid JSON, or not a JSON object.
    InvalidJson,
    /// A field in the body does not match the permit's body rule
    /// (wrong session / duration / order type / instruction / asset
    /// type, wrong leg count, or an extra field that is not in the
    /// rule — unknown fields are rejected).
    BodyMismatch(String),
    /// The symbol is not in the allowlist.
    SymbolNotAllowlisted(String),
    /// quantity * live quote price exceeds the per-run cap, or no
    /// live price is available to check against.
    OverCap(String),
}

impl DenyReason {
    pub fn code(&self) -> &'static str {
        match self {
            DenyReason::NoRoute => "no_route",
            DenyReason::InvalidJson => "invalid_json",
            DenyReason::BodyMismatch(_) => "body_mismatch",
            DenyReason::SymbolNotAllowlisted(_) => "symbol_not_allowlisted",
            DenyReason::OverCap(_) => "over_cap",
        }
    }
    pub fn message(&self) -> String {
        match self {
            DenyReason::NoRoute => "no permit matches this method and path".into(),
            DenyReason::InvalidJson => "request body is not valid JSON".into(),
            DenyReason::BodyMismatch(m) => format!("body does not match the permit: {m}"),
            DenyReason::SymbolNotAllowlisted(s) => format!("symbol {s} is not allowlisted"),
            DenyReason::OverCap(m) => m.clone(),
        }
    }
}

/// The outcome of evaluating one request against the policy.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Decision {
    /// Forward it. Carries the matched action, e.g. `place_order`.
    Allow { action: String },
    Deny(DenyReason),
}

/// A parsed order body, after the permit's body rule has matched.
#[derive(Debug, Clone)]
#[allow(dead_code)] // quantity is enforced in evaluate(); kept for audit/logging
pub struct OrderBody {
    pub symbol: String,
    pub quantity: i64,
    /// Path parameters captured from the template, e.g. `hash`.
    pub path_params: Vec<(String, String)>,
}

/// Match a request path against a permit path template. Literal
/// segments must match exactly; `{name}` captures exactly one
/// segment. Segment count must match. No `*`, no `**`, no regex.
pub fn match_path(template: &str, path: &str) -> Option<Vec<(String, String)>> {
    let t: Vec<&str> = template.split('/').collect();
    let p: Vec<&str> = path.split('/').collect();
    if t.len() != p.len() {
        return None;
    }
    let mut params = Vec::new();
    for (ts, ps) in t.iter().zip(p.iter()) {
        if let Some(name) = ts.strip_prefix('{').and_then(|s| s.strip_suffix('}')) {
            if ps.is_empty() {
                return None;
            }
            params.push((name.to_string(), ps.to_string()));
        } else if ts != ps {
            return None;
        }
    }
    Some(params)
}

/// Evaluate one request. `body` is the raw request body (empty for
/// GETs). `price` is the last live quote price for the order's symbol,
/// if the script has quoted it first — required to enforce the
/// dollar cap on `place_order` / `preview_order`.
pub fn evaluate(
    policy: &PolicySet,
    allowlist: &Allowlist,
    method: &str,
    path: &str,
    query: &str,
    body: &[u8],
    price: Option<f64>,
) -> (Decision, Option<OrderBody>) {
    // 1. Find a permit by method + path template.
    let mut matched = None;
    let mut params = Vec::new();
    for permit in &policy.permits {
        if permit.method != method {
            continue;
        }
        if let Some(p) = match_path(&permit.path, path) {
            matched = Some(permit);
            params = p;
            break;
        }
    }
    let permit = match matched {
        Some(p) => p,
        None => return (Decision::Deny(DenyReason::NoRoute), None),
    };

    // 2. Query allowlist: only `symbols` is allowed on the quote
    //    route, and only with a value. Anything else is denied.
    if permit.action == "get_quote" {
        if !query.is_empty() {
            let ok = query
                .split('&')
                .all(|kv| kv.starts_with("symbols=") && kv.len() > "symbols=".len());
            if !ok {
                return (
                    Decision::Deny(DenyReason::BodyMismatch(
                        "only the symbols query parameter is allowed".into(),
                    )),
                    None,
                );
            }
        }
        return (Decision::Allow { action: permit.action.clone() }, None);
    }

    // 3. GETs with no body rule need nothing more.
    let rule = match &permit.body {
        Some(r) => r,
        None => {
            if !body.is_empty() {
                return (
                    Decision::Deny(DenyReason::BodyMismatch(
                        "this route takes no body".into(),
                    )),
                    None,
                );
            }
            return (Decision::Allow { action: permit.action.clone() }, None);
        }
    };

    // 4. Body rule: parse JSON, reject unknown fields, check every
    //    field against the rule. Exactly one leg, instruction BUY.
    let value: Value = match serde_json::from_slice(body) {
        Ok(v) => v,
        Err(_) => return (Decision::Deny(DenyReason::InvalidJson), None),
    };
    let obj = match value.as_object() {
        Some(o) => o,
        None => return (Decision::Deny(DenyReason::InvalidJson), None),
    };
    let allowed_keys = ["session", "duration", "orderType", "orderStrategyType", "orderLegCollection"];
    for key in obj.keys() {
        if !allowed_keys.contains(&key.as_str()) {
            return (
                Decision::Deny(DenyReason::BodyMismatch(format!("unknown field {key}"))),
                None,
            );
        }
    }
    let get_str = |k: &str| obj.get(k).and_then(|v| v.as_str());
    if get_str("session") != Some(rule.session.as_str()) {
        return (
            Decision::Deny(DenyReason::BodyMismatch("session must be NORMAL".into())),
            None,
        );
    }
    if get_str("duration") != Some(rule.duration.as_str()) {
        return (
            Decision::Deny(DenyReason::BodyMismatch("duration must be DAY".into())),
            None,
        );
    }
    if get_str("orderStrategyType") != Some(rule.order_strategy_type.as_str()) {
        return (
            Decision::Deny(DenyReason::BodyMismatch("orderStrategyType must be SINGLE".into())),
            None,
        );
    }
    let order_type = get_str("orderType");
    match order_type {
        Some(t) if rule.order_types.iter().any(|x| x == t) => {}
        _ => {
            return (
                Decision::Deny(DenyReason::BodyMismatch(
                    "orderType must be MARKET or LIMIT".into(),
                )),
                None,
            );
        }
    }
    let legs = match obj.get("orderLegCollection").and_then(|v| v.as_array()) {
        Some(l) if l.len() == 1 => l,
        _ => {
            return (
                Decision::Deny(DenyReason::BodyMismatch(
                    "orderLegCollection must have exactly 1 leg".into(),
                )),
                None,
            );
        }
    };
    let leg = legs[0].as_object().cloned().unwrap_or_default();
    for key in leg.keys() {
        if !["instruction", "quantity", "instrument"].contains(&key.as_str()) {
            return (
                Decision::Deny(DenyReason::BodyMismatch(format!("unknown leg field {key}"))),
                None,
            );
        }
    }
    if leg.get("instruction").and_then(|v| v.as_str()) != Some(rule.instruction.as_str()) {
        return (
            Decision::Deny(DenyReason::BodyMismatch("instruction must be BUY".into())),
            None,
        );
    }
    let quantity = match leg.get("quantity").and_then(|v| v.as_i64()) {
        Some(q) if q >= 1 => q,
        _ => {
            return (
                Decision::Deny(DenyReason::BodyMismatch("quantity must be >= 1".into())),
                None,
            );
        }
    };
    let instrument = leg.get("instrument").and_then(|v| v.as_object());
    let (symbol, asset_type) = match instrument {
        Some(i) => (
            i.get("symbol").and_then(|v| v.as_str()).unwrap_or("").to_string(),
            i.get("assetType").and_then(|v| v.as_str()).unwrap_or("").to_string(),
        ),
        None => {
            return (
                Decision::Deny(DenyReason::BodyMismatch("instrument is required".into())),
                None,
            );
        }
    };
    if !rule.asset_types.iter().any(|x| *x == asset_type) {
        return (
            Decision::Deny(DenyReason::BodyMismatch(
                "assetType must be EQUITY or ETF".into(),
            )),
            None,
        );
    }
    if !allowlist.symbols.iter().any(|s| *s == symbol) {
        return (
            Decision::Deny(DenyReason::SymbolNotAllowlisted(symbol)),
            None,
        );
    }

    // 5. Dollar cap: quantity * live price <= cap. No price (the
    //    script did not quote first) is a deny, not a pass.
    let cap = permit.max_dollars.unwrap_or(allowlist.max_dollars_per_run);
    match price {
        Some(p) if (quantity as f64) * p <= cap as f64 => {}
        Some(p) => {
            return (
                Decision::Deny(DenyReason::OverCap(format!(
                    "quantity {quantity} x price {p} exceeds cap ${cap}"
                ))),
                None,
            );
        }
        None => {
            return (
                Decision::Deny(DenyReason::OverCap(
                    "no live quote price available — quote the symbol first".into(),
                )),
                None,
            );
        }
    }

    (
        Decision::Allow { action: permit.action.clone() },
        Some(OrderBody { symbol, quantity, path_params: params }),
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::{Allowlist, PolicySet};

    fn load() -> (PolicySet, Allowlist) {
        (
            PolicySet::load_from_str(include_str!("../../config/generated/policies.json")).unwrap(),
            Allowlist::load_from_str(include_str!("../../config/generated/allowlist.json")).unwrap(),
        )
    }

    fn buy_body(symbol: &str, instruction: &str) -> Vec<u8> {
        serde_json::json!({
            "session": "NORMAL",
            "duration": "DAY",
            "orderType": "LIMIT",
            "orderStrategyType": "SINGLE",
            "orderLegCollection": [{
                "instruction": instruction,
                "quantity": 1,
                "instrument": {"symbol": symbol, "assetType": "ETF"}
            }]
        })
        .to_string()
        .into_bytes()
    }

    #[test]
    fn allowed_get_quote() {
        let (p, a) = load();
        let (d, _) = evaluate(&p, &a, "GET", "/marketdata/v1/quotes", "symbols=VOO", b"", None);
        assert_eq!(d, Decision::Allow { action: "get_quote".into() });
    }

    #[test]
    fn allowed_place_order_with_price() {
        let (p, a) = load();
        let body = buy_body("VOO", "BUY");
        let (d, o) = evaluate(&p, &a, "POST", "/trader/v1/accounts/abc123/orders", "", &body, Some(500.0));
        assert_eq!(d, Decision::Allow { action: "place_order".into() });
        assert_eq!(o.unwrap().symbol, "VOO");
    }

    #[test]
    fn sell_is_denied() {
        let (p, a) = load();
        let body = buy_body("VOO", "SELL");
        let (d, _) = evaluate(&p, &a, "POST", "/trader/v1/accounts/abc123/orders", "", &body, Some(500.0));
        assert!(matches!(d, Decision::Deny(DenyReason::BodyMismatch(_))));
    }

    #[test]
    fn non_allowlisted_symbol_is_denied() {
        let (p, a) = load();
        let body = buy_body("GME", "BUY");
        let (d, _) = evaluate(&p, &a, "POST", "/trader/v1/accounts/abc123/orders", "", &body, Some(10.0));
        assert!(matches!(d, Decision::Deny(DenyReason::SymbolNotAllowlisted(_))));
    }

    #[test]
    fn over_cap_is_denied() {
        let (p, a) = load();
        let body = buy_body("VOO", "BUY");
        let (d, _) = evaluate(&p, &a, "POST", "/trader/v1/accounts/abc123/orders", "", &body, Some(5000.0));
        assert!(matches!(d, Decision::Deny(DenyReason::OverCap(_))));
    }

    #[test]
    fn no_price_is_denied_for_place_order() {
        let (p, a) = load();
        let body = buy_body("VOO", "BUY");
        let (d, _) = evaluate(&p, &a, "POST", "/trader/v1/accounts/abc123/orders", "", &body, None);
        assert!(matches!(d, Decision::Deny(DenyReason::OverCap(_))));
    }

    #[test]
    fn extra_field_is_denied() {
        let (p, a) = load();
        let mut body: serde_json::Value =
            serde_json::from_slice(&buy_body("VOO", "BUY")).unwrap();
        body["surprise"] = serde_json::json!("x");
        let (d, _) = evaluate(
            &p, &a, "POST", "/trader/v1/accounts/abc123/orders", "", body.to_string().as_bytes(), Some(500.0),
        );
        assert!(matches!(d, Decision::Deny(DenyReason::BodyMismatch(_))));
    }

    #[test]
    fn delete_and_put_have_no_route() {
        let (p, a) = load();
        let (d, _) = evaluate(&p, &a, "DELETE", "/trader/v1/accounts/abc123/orders/1", "", b"", None);
        assert_eq!(d, Decision::Deny(DenyReason::NoRoute));
        let (d, _) = evaluate(&p, &a, "PUT", "/trader/v1/accounts/abc123/orders/1", "", b"", None);
        assert_eq!(d, Decision::Deny(DenyReason::NoRoute));
    }

    #[test]
    fn invalid_json_is_denied() {
        let (p, a) = load();
        let (d, _) = evaluate(
            &p, &a, "POST", "/trader/v1/accounts/abc123/orders", "", b"{not json", Some(500.0),
        );
        assert_eq!(d, Decision::Deny(DenyReason::InvalidJson));
    }

    #[test]
    fn wrong_session_is_denied() {
        let (p, a) = load();
        let mut body: serde_json::Value =
            serde_json::from_slice(&buy_body("VOO", "BUY")).unwrap();
        body["session"] = serde_json::json!("EXTENDED");
        let (d, _) = evaluate(
            &p, &a, "POST", "/trader/v1/accounts/abc123/orders", "", body.to_string().as_bytes(), Some(500.0),
        );
        assert!(matches!(d, Decision::Deny(DenyReason::BodyMismatch(_))));
    }

    #[test]
    fn path_params_capture_hash() {
        assert_eq!(
            match_path("/trader/v1/accounts/{hash}/orders", "/trader/v1/accounts/abc123/orders"),
            Some(vec![("hash".to_string(), "abc123".to_string())])
        );
        assert_eq!(match_path("/trader/v1/accounts/{hash}/orders", "/trader/v1/accounts/a/b/orders"), None);
    }
}
