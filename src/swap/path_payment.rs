//! Stellar-native swap execution via `PathPaymentStrictSend` /
//! `PathPaymentStrictReceive`.
//!
//! Horizon path finding is a read-only quote source ([`HorizonPathFinder`]);
//! the resulting [`PathPaymentOp`] is the executable venue. Quote and execution
//! are decoupled: a stale path is refreshed by calling [`HorizonPathFinder`]
//! again, never blindly resubmitted. Destination accounts and memos are
//! deliberately excluded from `Debug` output and logs.
//!
//! Amounts are integer stroops (1 unit = 10^7 stroops).

use serde::Deserialize;

/// Stellar allows at most 5 intermediate hops in a path payment.
pub const MAX_HOPS: usize = 5;
const STROOPS: i128 = 10_000_000;
const BPS: i128 = 10_000;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PathError {
    NoPath,
    TooManyHops(usize),
    /// Best available route would fill below the quoted floor / above the cap.
    BelowQuotedFloor,
    InvalidAmount(String),
    Horizon(String),
    /// Submission result `op_under_dest_min`: path moved; refresh the quote.
    UnderDestMin,
    /// Submission result `op_too_few_offers`: liquidity vanished; reroute.
    TooFewOffers,
    OtherOpFailure(String),
}

impl std::fmt::Display for PathError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "path payment error: {self:?}")
    }
}

impl std::error::Error for PathError {}

impl PathError {
    /// Whether a refreshed quote may succeed (vs. a terminal failure).
    pub fn is_retryable_with_refresh(&self) -> bool {
        matches!(
            self,
            PathError::UnderDestMin | PathError::TooFewOffers | PathError::Horizon(_)
        )
    }

    /// Classify a Horizon `result_codes.operations[0]` string.
    pub fn from_result_code(code: &str) -> PathError {
        match code {
            "op_under_dest_min" => PathError::UnderDestMin,
            "op_too_few_offers" => PathError::TooFewOffers,
            other => PathError::OtherOpFailure(other.to_string()),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub struct Asset {
    pub asset_type: String,
    #[serde(default)]
    pub asset_code: Option<String>,
    #[serde(default)]
    pub asset_issuer: Option<String>,
}

impl Asset {
    pub fn canonical(&self) -> String {
        match (&self.asset_code, &self.asset_issuer) {
            (Some(c), Some(i)) => format!("{c}:{i}"),
            _ => "native".to_string(),
        }
    }
}

#[derive(Debug, Clone, Deserialize)]
struct PathRecord {
    source_amount: String,
    destination_amount: String,
    path: Vec<Asset>,
}

#[derive(Debug, Deserialize)]
struct Embedded {
    records: Vec<PathRecord>,
}

#[derive(Debug, Deserialize)]
struct PathsResponse {
    #[serde(rename = "_embedded")]
    embedded: Embedded,
}

/// A route returned by Horizon, with amounts in stroops.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Route {
    pub source_amount: i128,
    pub destination_amount: i128,
    pub hops: Vec<Asset>,
}

/// Parse a decimal amount string with up to 7 fractional digits into stroops.
pub fn parse_stroops(s: &str) -> Result<i128, PathError> {
    let bad = || PathError::InvalidAmount(s.to_string());
    let (int, frac) = s.split_once('.').unwrap_or((s, ""));
    if frac.len() > 7 || int.is_empty() {
        return Err(bad());
    }
    let int: i128 = int.parse().map_err(|_| bad())?;
    let frac: i128 = format!("{frac:0<7}").parse().map_err(|_| bad())?;
    Ok(int * STROOPS + frac)
}

fn to_routes(body: &str) -> Result<Vec<Route>, PathError> {
    let parsed: PathsResponse =
        serde_json::from_str(body).map_err(|e| PathError::Horizon(e.to_string()))?;
    parsed
        .embedded
        .records
        .into_iter()
        .map(|r| {
            if r.path.len() > MAX_HOPS {
                return Err(PathError::TooManyHops(r.path.len()));
            }
            Ok(Route {
                source_amount: parse_stroops(&r.source_amount)?,
                destination_amount: parse_stroops(&r.destination_amount)?,
                hops: r.path,
            })
        })
        .collect()
}

/// The quote's slippage / minimum-received policy.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct QuotePolicy {
    pub slippage_bps: u32,
    /// Quoted floor for strict-send; ceiling on `send_max` for strict-receive.
    pub bound: i128,
}

/// Executable path-payment operation. `Debug` omits `destination`.
#[derive(Clone, PartialEq, Eq)]
pub enum PathPaymentOp {
    StrictSend {
        send_amount: i128,
        destination_min: i128,
        destination: String,
        path: Vec<Asset>,
    },
    StrictReceive {
        destination_amount: i128,
        send_max: i128,
        destination: String,
        path: Vec<Asset>,
    },
}

impl std::fmt::Debug for PathPaymentOp {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::StrictSend {
                send_amount,
                destination_min,
                path,
                ..
            } => f
                .debug_struct("StrictSend")
                .field("send_amount", send_amount)
                .field("destination_min", destination_min)
                .field("hops", &path.len())
                .finish_non_exhaustive(),
            Self::StrictReceive {
                destination_amount,
                send_max,
                path,
                ..
            } => f
                .debug_struct("StrictReceive")
                .field("destination_amount", destination_amount)
                .field("send_max", send_max)
                .field("hops", &path.len())
                .finish_non_exhaustive(),
        }
    }
}

impl PathPaymentOp {
    /// Concrete hops for the fill record (`swap_fills.path`).
    pub fn path_for_record(&self) -> Vec<String> {
        let path = match self {
            Self::StrictSend { path, .. } | Self::StrictReceive { path, .. } => path,
        };
        path.iter().map(Asset::canonical).collect()
    }
}

/// Strict-send: `destination_min` = quoted route output less slippage, never
/// below the quoted floor. Picks the route with the highest output.
pub fn build_strict_send(
    routes: &[Route],
    policy: QuotePolicy,
    destination: &str,
) -> Result<PathPaymentOp, PathError> {
    let best = routes
        .iter()
        .max_by_key(|r| r.destination_amount)
        .ok_or(PathError::NoPath)?;
    let after_slippage = best.destination_amount * (BPS - policy.slippage_bps as i128) / BPS;
    let destination_min = after_slippage.max(policy.bound);
    if best.destination_amount < policy.bound {
        return Err(PathError::BelowQuotedFloor);
    }
    Ok(PathPaymentOp::StrictSend {
        send_amount: best.source_amount,
        destination_min,
        destination: destination.to_string(),
        path: best.hops.clone(),
    })
}

/// Strict-receive: `send_max` = route cost plus slippage (rounded up), which
/// must not exceed the quoted cap. Picks the cheapest route.
pub fn build_strict_receive(
    routes: &[Route],
    policy: QuotePolicy,
    destination: &str,
) -> Result<PathPaymentOp, PathError> {
    let best = routes
        .iter()
        .min_by_key(|r| r.source_amount)
        .ok_or(PathError::NoPath)?;
    let send_max = (best.source_amount * (BPS + policy.slippage_bps as i128) + BPS - 1) / BPS;
    if send_max > policy.bound {
        return Err(PathError::BelowQuotedFloor);
    }
    Ok(PathPaymentOp::StrictReceive {
        destination_amount: best.destination_amount,
        send_max,
        destination: destination.to_string(),
        path: best.hops.clone(),
    })
}

fn asset_params(prefix: &str, a: &Asset) -> Vec<(String, String)> {
    let mut p = vec![(format!("{prefix}_asset_type"), a.asset_type.clone())];
    if let (Some(c), Some(i)) = (&a.asset_code, &a.asset_issuer) {
        p.push((format!("{prefix}_asset_code"), c.clone()));
        p.push((format!("{prefix}_asset_issuer"), i.clone()));
    }
    p
}

fn fmt_amount(stroops: i128) -> String {
    format!("{}.{:07}", stroops / STROOPS, stroops % STROOPS)
}

/// Read-only Horizon path-finding quote source.
pub struct HorizonPathFinder {
    base: String,
    client: reqwest::Client,
}

impl HorizonPathFinder {
    pub fn new(horizon_url: &str) -> Self {
        Self {
            base: horizon_url.trim_end_matches('/').to_string(),
            client: reqwest::Client::builder()
                .timeout(std::time::Duration::from_secs(10))
                .build()
                .expect("reqwest client"),
        }
    }

    async fn fetch(
        &self,
        endpoint: &str,
        params: Vec<(String, String)>,
    ) -> Result<Vec<Route>, PathError> {
        let resp = self
            .client
            .get(format!("{}/paths/{endpoint}", self.base))
            .query(&params)
            .send()
            .await
            .map_err(|e| PathError::Horizon(e.without_url().to_string()))?;
        match resp.status().as_u16() {
            429 => return Err(PathError::Horizon("rate limited".into())),
            s if !(200..300).contains(&s) => return Err(PathError::Horizon(format!("status {s}"))),
            _ => {}
        }
        let body = resp
            .text()
            .await
            .map_err(|e| PathError::Horizon(e.without_url().to_string()))?;
        let routes = to_routes(&body)?;
        if routes.is_empty() {
            return Err(PathError::NoPath);
        }
        Ok(routes)
    }

    /// Routes that send exactly `source_amount` of `source` to `destination_asset`.
    pub async fn strict_send_routes(
        &self,
        source: &Asset,
        source_amount: i128,
        destination_asset: &Asset,
    ) -> Result<Vec<Route>, PathError> {
        let mut p = asset_params("source", source);
        p.push(("source_amount".into(), fmt_amount(source_amount)));
        p.push((
            "destination_assets".into(),
            match (
                &destination_asset.asset_code,
                &destination_asset.asset_issuer,
            ) {
                (Some(c), Some(i)) => format!("{c}:{i}"),
                _ => "native".into(),
            },
        ));
        self.fetch("strict-send", p).await
    }

    /// Routes that deliver exactly `destination_amount` of `destination_asset`.
    pub async fn strict_receive_routes(
        &self,
        source_assets: &[Asset],
        destination_asset: &Asset,
        destination_amount: i128,
    ) -> Result<Vec<Route>, PathError> {
        let mut p = asset_params("destination", destination_asset);
        p.push(("destination_amount".into(), fmt_amount(destination_amount)));
        p.push((
            "source_assets".into(),
            source_assets
                .iter()
                .map(Asset::canonical)
                .collect::<Vec<_>>()
                .join(","),
        ));
        self.fetch("strict-receive", p).await
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn usdc() -> Asset {
        Asset {
            asset_type: "credit_alphanum4".into(),
            asset_code: Some("USDC".into()),
            asset_issuer: Some("GISSUER".into()),
        }
    }
    fn native() -> Asset {
        Asset {
            asset_type: "native".into(),
            asset_code: None,
            asset_issuer: None,
        }
    }
    fn route(src: i128, dst: i128, hops: Vec<Asset>) -> Route {
        Route {
            source_amount: src,
            destination_amount: dst,
            hops,
        }
    }
    const POLICY: QuotePolicy = QuotePolicy {
        slippage_bps: 100,
        bound: 9_000_000,
    };

    #[test]
    fn parses_stroops() {
        assert_eq!(parse_stroops("1.5").unwrap(), 15_000_000);
        assert_eq!(parse_stroops("0.0000001").unwrap(), 1);
        assert_eq!(parse_stroops("10").unwrap(), 100_000_000);
        assert!(parse_stroops("1.00000001").is_err());
        assert!(parse_stroops("abc").is_err());
        assert_eq!(fmt_amount(15_000_000), "1.5000000");
    }

    #[test]
    fn strict_send_min_never_below_floor() {
        let op =
            build_strict_send(&[route(10_000_000, 9_500_000, vec![])], POLICY, "GDEST").unwrap();
        // 9_500_000 * 99% = 9_405_000 > floor
        assert!(matches!(
            op,
            PathPaymentOp::StrictSend {
                destination_min: 9_405_000,
                ..
            }
        ));
        let tight = QuotePolicy {
            slippage_bps: 1_000,
            bound: 9_400_000,
        };
        let op =
            build_strict_send(&[route(10_000_000, 9_500_000, vec![])], tight, "GDEST").unwrap();
        assert!(matches!(
            op,
            PathPaymentOp::StrictSend {
                destination_min: 9_400_000,
                ..
            }
        ));
    }

    #[test]
    fn refuses_route_below_floor_or_over_cap() {
        assert_eq!(
            build_strict_send(&[route(10_000_000, 8_000_000, vec![])], POLICY, "G"),
            Err(PathError::BelowQuotedFloor)
        );
        let cap = QuotePolicy {
            slippage_bps: 100,
            bound: 10_000_000,
        };
        assert_eq!(
            build_strict_receive(&[route(10_000_000, 9_000_000, vec![])], cap, "G"),
            Err(PathError::BelowQuotedFloor)
        );
        assert_eq!(build_strict_send(&[], POLICY, "G"), Err(PathError::NoPath));
    }

    #[test]
    fn strict_receive_rounds_send_max_up_and_picks_cheapest() {
        let cap = QuotePolicy {
            slippage_bps: 100,
            bound: 20_000_000,
        };
        let routes = [
            route(10_000_001, 9_000_000, vec![usdc()]),
            route(11_000_000, 9_000_000, vec![]),
        ];
        let op = build_strict_receive(&routes, cap, "G").unwrap();
        // ceil(10_000_001 * 1.01) = 10_100_002
        assert!(matches!(
            &op,
            PathPaymentOp::StrictReceive {
                send_max: 10_100_002,
                ..
            }
        ));
        assert_eq!(op.path_for_record(), vec!["USDC:GISSUER".to_string()]);
    }

    #[test]
    fn debug_hides_destination() {
        let op = build_strict_send(
            &[route(1, 9_500_000, vec![native()])],
            POLICY,
            "GSECRETDEST",
        )
        .unwrap();
        assert!(!format!("{op:?}").contains("GSECRETDEST"));
    }

    #[test]
    fn classifies_failures() {
        assert_eq!(
            PathError::from_result_code("op_under_dest_min"),
            PathError::UnderDestMin
        );
        assert_eq!(
            PathError::from_result_code("op_too_few_offers"),
            PathError::TooFewOffers
        );
        assert!(PathError::UnderDestMin.is_retryable_with_refresh());
        assert!(!PathError::from_result_code("op_malformed").is_retryable_with_refresh());
    }

    fn body(hops: usize) -> String {
        let path: Vec<_> = (0..hops)
            .map(|_| serde_json::json!({"asset_type":"native"}))
            .collect();
        serde_json::json!({"_embedded":{"records":[
            {"source_amount":"1.0000000","destination_amount":"0.9500000","path":path}
        ]}})
        .to_string()
    }

    #[tokio::test]
    async fn horizon_success_missing_path_and_error() {
        let mut server = mockito::Server::new_async().await;
        let finder = HorizonPathFinder::new(&server.url());
        let ok = server
            .mock("GET", mockito::Matcher::Any)
            .with_body(body(1))
            .create_async()
            .await;
        let routes = finder
            .strict_send_routes(&native(), 10_000_000, &usdc())
            .await
            .unwrap();
        assert_eq!(routes[0].destination_amount, 9_500_000);
        ok.remove_async().await;

        let empty = server
            .mock("GET", mockito::Matcher::Any)
            .with_body(r#"{"_embedded":{"records":[]}}"#)
            .create_async()
            .await;
        assert_eq!(
            finder.strict_send_routes(&native(), 1, &usdc()).await,
            Err(PathError::NoPath)
        );
        empty.remove_async().await;

        let _err = server
            .mock("GET", mockito::Matcher::Any)
            .with_status(429)
            .create_async()
            .await;
        let e = finder
            .strict_receive_routes(&[native()], &usdc(), 1)
            .await
            .unwrap_err();
        assert!(e.is_retryable_with_refresh());
    }

    #[test]
    fn too_many_hops_rejected() {
        assert_eq!(to_routes(&body(6)), Err(PathError::TooManyHops(6)));
        assert!(to_routes(&body(5)).is_ok());
    }

    #[tokio::test]
    async fn horizon_timeout_is_retryable() {
        // Nothing listens on this port: connection failure maps to Horizon error.
        let finder = HorizonPathFinder::new("http://127.0.0.1:9");
        let e = finder
            .strict_send_routes(&native(), 1, &usdc())
            .await
            .unwrap_err();
        assert!(matches!(e, PathError::Horizon(_)));
    }
}
