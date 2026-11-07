//! Configuration types: the Rust mirror of the Pkl schemas.
//!
//! `config/schemas/PolicySet.pkl` and `config/schemas/SchwabAllowlist.pkl`
//! are the source of truth. These serde structs must match them field
//! for field; a CI check deserializes the Pkl-rendered JSON in
//! `config/generated/` into these structs, so Pkl/Rust drift fails
//! the build instead of failing at 2am during a run.
//!
//! Loading is strict and fail-closed: unknown fields are rejected
//! (`deny_unknown_fields`), so a typo'd or extra field in the rendered
//! JSON refuses to start the proxy instead of being silently ignored.

use serde::Deserialize;

/// One permit: a single (method, path template) the script may call,
/// optionally with a body rule and a dollar cap.
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Permit {
    pub id: String,
    /// Closed union in Pkl: exactly the 7 actions. Anything else is a
    /// Pkl compile error; here it is a load error.
    pub action: String,
    pub method: String,
    /// Path template: literal segments, or a single-segment parameter
    /// in braces, e.g. `/trader/v1/accounts/{hash}/orders`.
    pub path: String,
    #[serde(default)]
    pub body: Option<OrderBodyRule>,
    #[serde(rename = "maxDollars", default)]
    pub max_dollars: Option<i64>,
}

/// The body *rule* for an order request. This is a rule, not a concrete
/// order: it says which values are allowed. The symbol is checked
/// against the allowlist, the quantity must be >= 1, and the dollar
/// cap is checked with a live quote price — all in `policy.rs`.
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct OrderBodyRule {
    pub session: String,
    pub duration: String,
    #[serde(rename = "orderTypes")]
    pub order_types: Vec<String>,
    #[serde(rename = "orderStrategyType")]
    pub order_strategy_type: String,
    pub instruction: String,
    #[serde(rename = "assetTypes")]
    pub asset_types: Vec<String>,
}

/// The whole policy: the only way to allow something is to list it
/// here. An empty list denies everything.
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PolicySet {
    pub permits: Vec<Permit>,
}

/// Named sets the permits refer to: which symbols may be bought, how
/// much may be spent in one run, how long the rolling window is.
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Allowlist {
    pub symbols: Vec<String>,
    #[serde(rename = "maxDollarsPerRun")]
    pub max_dollars_per_run: i64,
    #[serde(rename = "rollingWindowDays")]
    pub rolling_window_days: i64,
}

impl PolicySet {
    /// Load and validate the rendered Pkl JSON. Fails closed: any
    /// parse error, duplicate action, unknown action, or empty permit
    /// list is an error and the proxy must refuse to start.
    pub fn load_from_str(json: &str) -> Result<Self, String> {
        let set: PolicySet =
            serde_json::from_str(json).map_err(|e| format!("policies.json: {e}"))?;
        if set.permits.is_empty() {
            return Err("policies.json: permits list is empty, everything would be denied".into());
        }
        let allowed = [
            "get_account_numbers",
            "get_account",
            "list_orders",
            "get_order",
            "get_quote",
            "preview_order",
            "place_order",
        ];
        let mut seen = std::collections::HashSet::new();
        for p in &set.permits {
            if !allowed.contains(&p.action.as_str()) {
                return Err(format!("policies.json: unknown action {:?}", p.action));
            }
            if !seen.insert(p.action.clone()) {
                return Err(format!("policies.json: duplicate action {:?}", p.action));
            }
            if p.method != "GET" && p.method != "POST" {
                return Err(format!("policies.json: bad method {:?} in {}", p.method, p.id));
            }
            if !p.path.starts_with("/trader/v1/") && !p.path.starts_with("/marketdata/v1/") {
                return Err(format!("policies.json: bad path {:?} in {}", p.path, p.id));
            }
        }
        Ok(set)
    }
}

impl Allowlist {
    pub fn load_from_str(json: &str) -> Result<Self, String> {
        let list: Allowlist =
            serde_json::from_str(json).map_err(|e| format!("allowlist.json: {e}"))?;
        if list.symbols.is_empty() {
            return Err("allowlist.json: symbols list is empty".into());
        }
        if list.max_dollars_per_run <= 0 || list.rolling_window_days <= 0 {
            return Err("allowlist.json: maxDollarsPerRun / rollingWindowDays must be > 0".into());
        }
        Ok(list)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const POLICIES: &str = include_str!("../../config/generated/policies.json");
    const ALLOWLIST: &str = include_str!("../../config/generated/allowlist.json");

    #[test]
    fn generated_policies_parse_and_validate() {
        let set = PolicySet::load_from_str(POLICIES).unwrap();
        assert_eq!(set.permits.len(), 7);
        assert!(set.permits.iter().any(|p| p.action == "place_order"));
    }

    #[test]
    fn generated_allowlist_parses() {
        let list = Allowlist::load_from_str(ALLOWLIST).unwrap();
        assert!(list.symbols.contains(&"VOO".to_string()));
        assert_eq!(list.max_dollars_per_run, 1000);
    }

    #[test]
    fn unknown_action_is_rejected() {
        let bad = r#"{"permits":[{"id":"x","action":"sell","method":"POST","path":"/trader/v1/accounts/{hash}/orders"}]}"#;
        assert!(PolicySet::load_from_str(bad).is_err());
    }

    #[test]
    fn unknown_field_is_rejected() {
        // deny_unknown_fields: a typo'd / extra field must fail load,
        // not be silently ignored.
        let bad = r#"{"permits":[{"id":"x","action":"get_quote","method":"GET","path":"/marketdata/v1/quotes","forbid":true}]}"#;
        assert!(PolicySet::load_from_str(bad).is_err());
    }

    #[test]
    fn empty_permits_is_rejected() {
        assert!(PolicySet::load_from_str(r#"{"permits":[]}"#).is_err());
    }
}
