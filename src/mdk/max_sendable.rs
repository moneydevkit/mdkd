//! Estimator for the largest amount that can be sent over Lightning
//! out of mdk's LSP channel(s), with routing fees subtracted.
//!
//! Entry point: [`compute_estimate`]. The caller collects channel
//! state and provides a `find_route` closure plus an `HrnResolver`;
//! this module owns the `dest` dispatch and the choice between
//! buffer and route-based estimators.
//!
//! # Coverage
//!
//! | Destination                            | Behaviour                       |
//! |----------------------------------------|---------------------------------|
//! | None                                   | buffer                          |
//! | BOLT11, amount set by payee            | `Err(FixedAmount)`              |
//! | BOLT11, zero-amount                    | route-fee estimate              |
//! | BOLT12 offer                           | buffer (TODO)                   |
//! | LNURL-pay, balance >= min_value        | route-fee estimate              |
//! | LNURL-pay, balance < min_value         | `Err(BelowLnurlMin)`            |
//! | HRN (BIP 353)                          | dispatches as the resolved BIP  |
//! |                                        | 321 methods (typically BOLT12)  |
//! | HRN (LN address) -> LNURL-pay          | as LNURL-pay above              |
//! | Onchain only                           | `Err(NoLightningMethod)`        |
//! | `find_route` fails                     | `Err(RoutingFailure(msg))`      |
//! | LNURL fetch/validation fails           | `Err(LnurlResolutionFailed)`    |
//!
//! BOLT12 still falls back to the buffer until `from_bolt12_invoice`
//! lands upstream. HRNs are resolved at `PaymentInstructions::parse`
//! time: BIP 353 names resolve to a BIP 321 `bitcoin:` URI carrying
//! whatever reusable methods the recipient publishes (most commonly
//! a BOLT12 offer, sometimes BOLT11 or on-chain fallbacks), and
//! LN-Address names look like LNURL-pay. Whichever method
//! `pick_strategy` reaches first wins per the iteration order of
//! `methods()`.

use std::time::Instant;

use bitcoin_payment_instructions::amount::Amount as InstructionAmount;
use bitcoin_payment_instructions::hrn_resolution::HrnResolver;
use bitcoin_payment_instructions::{
    ConfigurableAmountPaymentInstructions, PaymentInstructions, PaymentMethod,
    PossiblyResolvedPaymentMethod,
};
use ldk_node::bitcoin::secp256k1::PublicKey;
use ldk_node::lightning_invoice::Bolt11Invoice as LdkBolt11Invoice;
use ldk_node::{ChannelDetails, PaymentParameters, Route, RouteParameters};

/// User-tunable buffers applied to the raw outbound liquidity (when
/// no destination is supplied) or to a destination-aware route's
/// computed fees, to reserve headroom for routing fees.
#[derive(Debug, Clone)]
pub struct MaxSendableConfig {
    /// Percentage buffer in basis points (1 bps = 0.01 %), applied
    /// to the outbound balance when no destination is supplied.
    /// Default: 100 (1 %).
    pub fee_buffer_bps: u16,
    /// Absolute lower bound on the no-destination buffer, in sats.
    /// Whichever of the percentage and the floor is larger wins.
    /// Default: 10.
    pub fee_buffer_floor_sats: u64,
    /// Fee budget multiplier in basis points of the cheapest route's
    /// computed `total_fees`, where 10_000 = 1.0x (no buffer) and
    /// 20_000 = 2.0x. Reserved so a send has headroom to retry along
    /// a more expensive path if the chosen route fails at payment
    /// time. Larger values raise payment success rate at the cost of
    /// a lower reported max sendable. Default: 11_000 (1.1x); bump
    /// up if production retries surface "insufficient fee budget".
    pub route_retry_fee_multiplier_bps: u16,
}

impl Default for MaxSendableConfig {
    fn default() -> Self {
        Self {
            fee_buffer_bps: 100,
            fee_buffer_floor_sats: 10,
            route_retry_fee_multiplier_bps: 11_000,
        }
    }
}

/// A best-effort estimate of how much can flow out over Lightning
/// right now, alongside the fee headroom the estimate carved out.
#[derive(Debug, Clone)]
pub struct MaxSendableEstimate {
    /// Amount to surface to the payer as "max sendable", in msat.
    /// Zero when the balance is fully consumed by the buffer (dust).
    pub amount_msat: u64,
    /// Subtracted from `balance_at_compute_msat` to reach `amount_msat`.
    /// Doubles as a hint for `max_total_routing_fee_msat`.
    pub fee_budget_msat: u64,
    /// Raw outbound liquidity at compute time (sum of usable LSP
    /// channels' `next_outbound_htlc_limit_msat`).
    pub balance_at_compute_msat: u64,
    pub computed_at: Instant,
}

#[derive(Debug, PartialEq, Eq)]
pub enum MaxSendableError {
    /// No usable LSP channel exists. Distinct from "balance is dust"
    /// (which returns `Ok(amount_msat: 0)`).
    NoUsableChannel,
    /// Payee dictated the amount; nothing to estimate. Carries the
    /// amount so the caller doesn't re-extract it.
    FixedAmount { amount_msat: u64 },
    /// `PaymentInstructions` carried no Lightning method (e.g. an
    /// on-chain-only `bitcoin:` URI).
    NoLightningMethod,
    /// `Node::find_route` failed. Lossy on purpose: the caller's
    /// only useful action is to retry or surface "no route".
    RoutingFailure(String),
    /// LNURL-pay callback fetch, validation, or BOLT11 round-trip
    /// failed. Wraps the underlying message verbatim.
    LnurlResolutionFailed(String),
    /// Current outbound balance is below the LNURL-pay endpoint's
    /// `min_value`, so no payment is possible. Distinct from
    /// `Ok(amount_msat = 0)` (dust against the fee buffer) so the
    /// caller can render a recipient-specific message such as
    /// "minimum send is N sats".
    BelowLnurlMin { min_msat: u64, balance_msat: u64 },
}

/// Minimal projection of `ldk_node::ChannelDetails` carrying only
/// the fields [`sum_outbound_balance`] looks at.
#[derive(Debug, Clone)]
pub(crate) struct ChannelSnapshot {
    pub counterparty: PublicKey,
    pub is_usable: bool,
    pub next_outbound_htlc_limit_msat: u64,
}

impl From<&ChannelDetails> for ChannelSnapshot {
    fn from(c: &ChannelDetails) -> Self {
        Self {
            counterparty: c.counterparty_node_id,
            is_usable: c.is_usable,
            next_outbound_htlc_limit_msat: c.next_outbound_htlc_limit_msat,
        }
    }
}

/// Top-level entry point. Picks an [`EstimationStrategy`] for
/// `dest` and folds the result into a [`MaxSendableEstimate`].
/// `find_route` and `resolver` are the only effects; everything
/// else is pure dispatch. `resolver` is invoked only when `dest`
/// carries an unresolved LNURL-pay endpoint. See the module-level
/// coverage table.
pub(crate) async fn compute_estimate<F, R>(
    dest: Option<&PaymentInstructions>,
    channels: &[ChannelSnapshot],
    lsp_pubkey: &PublicKey,
    cfg: &MaxSendableConfig,
    resolver: &R,
    find_route: F,
) -> Result<MaxSendableEstimate, MaxSendableError>
where
    F: FnOnce(RouteParameters) -> Result<Route, String>,
    R: HrnResolver,
{
    let balance_msat = sum_outbound_balance(channels, lsp_pubkey)?;
    match dest {
        None => Ok(subtract_fee_buffer(balance_msat, cfg)),
        Some(PaymentInstructions::FixedAmount(fixed)) => Err(MaxSendableError::FixedAmount {
            // `None` here means non-BTC pricing; report zero rather
            // than guess an FX rate.
            amount_msat: fixed
                .ln_payment_amount()
                .map(|a| a.milli_sats())
                .unwrap_or(0),
        }),
        Some(PaymentInstructions::ConfigurableAmount(inst)) => {
            match pick_strategy(inst, balance_msat, resolver).await? {
                EstimationStrategy::Buffer => Ok(subtract_fee_buffer(balance_msat, cfg)),
                EstimationStrategy::FromRoute(payment_params) => {
                    estimate_via_payment_params(balance_msat, payment_params, cfg, find_route)
                }
            }
        }
    }
}

/// Build `RouteParameters` from `payment_params` and `balance_msat`,
/// call `find_route`, then fold the resulting `Route` into a
/// `MaxSendableEstimate`. Extracted so the upcoming LNURL-pay branch
/// of [`compute_estimate`] can reuse the same routing tail after it
/// fetches the BOLT11 invoice.
fn estimate_via_payment_params<F>(
    balance_msat: u64,
    payment_params: PaymentParameters,
    cfg: &MaxSendableConfig,
    find_route: F,
) -> Result<MaxSendableEstimate, MaxSendableError>
where
    F: FnOnce(RouteParameters) -> Result<Route, String>,
{
    let route_params = RouteParameters::from_payment_params_and_value(payment_params, balance_msat);
    // TODO: Post channel consolidation, consider setting this to one to not use MPP
    // route_params.payment_params.max_path_count = 1;
    let route = find_route(route_params).map_err(MaxSendableError::RoutingFailure)?;
    Ok(estimate_from_route(balance_msat, &route, cfg))
}

/// Subtract the configured fee buffer from a known outbound balance.
fn subtract_fee_buffer(balance_msat: u64, cfg: &MaxSendableConfig) -> MaxSendableEstimate {
    // u128 intermediate dodges overflow at the percentage step.
    let pct_buffer = ((balance_msat as u128) * (cfg.fee_buffer_bps as u128) / 10_000) as u64;
    let floor_buffer = cfg.fee_buffer_floor_sats.saturating_mul(1_000);
    let buffer_msat = pct_buffer.max(floor_buffer);

    MaxSendableEstimate {
        amount_msat: balance_msat.saturating_sub(buffer_msat),
        fee_budget_msat: buffer_msat,
        balance_at_compute_msat: balance_msat,
        computed_at: Instant::now(),
    }
}

/// Estimate the max sendable amount from route fees. The multiplier scales
/// with the chosen route's cost so retries can pick a meaningfully more
/// expensive path.
fn estimate_from_route(
    balance_msat: u64,
    route: &Route,
    cfg: &MaxSendableConfig,
) -> MaxSendableEstimate {
    let total_fees_msat: u64 = route.paths.iter().map(|p| p.fee_msat()).sum();
    let fee_budget_msat =
        ((total_fees_msat as u128) * (cfg.route_retry_fee_multiplier_bps as u128) / 10_000) as u64;
    MaxSendableEstimate {
        amount_msat: balance_msat.saturating_sub(fee_budget_msat),
        fee_budget_msat,
        balance_at_compute_msat: balance_msat,
        computed_at: Instant::now(),
    }
}

/// Sum `next_outbound_htlc_limit_msat` over usable channels with
/// the LSP. The `Option<u64>` accumulator distinguishes "no channel
/// matched" (None → `NoUsableChannel`) from "channel(s) matched,
/// sum is 0" (Some(0) → dust).
fn sum_outbound_balance(
    channels: &[ChannelSnapshot],
    lsp_pubkey: &PublicKey,
) -> Result<u64, MaxSendableError> {
    channels
        .iter()
        .filter(|c| c.counterparty == *lsp_pubkey && c.is_usable)
        .fold(None::<u64>, |acc, c| {
            Some(
                acc.unwrap_or(0)
                    .saturating_add(c.next_outbound_htlc_limit_msat),
            )
        })
        .ok_or(MaxSendableError::NoUsableChannel)
}

/// How [`compute_estimate`] should price the chosen Lightning
/// destination — a decision, not a route.
#[derive(Debug)]
enum EstimationStrategy {
    /// Ask `find_route` with these params; subtract the real fees.
    FromRoute(PaymentParameters),
    /// Fall back to the simple buffer for destinations that cannot yet use
    /// route-based estimation.
    Buffer,
}

/// Pick the first Lightning method and decide how to price it.
/// `inst.methods()` yields resolved methods before LNURL, so a
/// destination carrying both a concrete BOLT11 and an LNURL
/// fallback prefers the BOLT11. On-chain methods are skipped; an
/// empty result yields `NoLightningMethod`.
///
/// LNURL-pay calls `set_amount` for `min(balance, max_value)` to
/// fetch the actual invoice the payer would receive at the largest
/// plausible amount, then prices it route-based. Sub-`min_value`
/// balances short-circuit to `Err(BelowLnurlMin)`.
async fn pick_strategy<R: HrnResolver>(
    inst: &ConfigurableAmountPaymentInstructions,
    balance_msat: u64,
    resolver: &R,
) -> Result<EstimationStrategy, MaxSendableError> {
    for method in inst.methods() {
        match method {
            PossiblyResolvedPaymentMethod::Resolved(PaymentMethod::LightningBolt11(inv)) => {
                return Ok(bolt11_to_strategy(&inv.to_string()));
            }
            PossiblyResolvedPaymentMethod::Resolved(PaymentMethod::LightningBolt12(_)) => {
                return Ok(EstimationStrategy::Buffer);
            }
            PossiblyResolvedPaymentMethod::LNURLPay {
                min_value,
                max_value,
                ..
            } => {
                return resolve_lnurl_strategy(inst, balance_msat, min_value, max_value, resolver)
                    .await;
            }
            _ => continue,
        }
    }
    Err(MaxSendableError::NoLightningMethod)
}

/// Round-trip a BOLT11 invoice string into ldk-node's `Bolt11Invoice`
/// type and wrap it in `PaymentParameters`.
fn bolt11_to_strategy(bolt11_str: &str) -> EstimationStrategy {
    match bolt11_str.parse::<LdkBolt11Invoice>() {
        Ok(ldk_inv) => {
            EstimationStrategy::FromRoute(PaymentParameters::from_bolt11_invoice(&ldk_inv))
        }
        Err(_) => EstimationStrategy::Buffer,
    }
}

/// Fetch a concrete BOLT11 invoice for an LNURL-pay endpoint at
/// `min(balance, max_value)`, then price it route-based.
async fn resolve_lnurl_strategy<R: HrnResolver>(
    inst: &ConfigurableAmountPaymentInstructions,
    balance_msat: u64,
    min_value: InstructionAmount,
    max_value: InstructionAmount,
    resolver: &R,
) -> Result<EstimationStrategy, MaxSendableError> {
    let min_msat = min_value.milli_sats();
    if balance_msat < min_msat {
        return Err(MaxSendableError::BelowLnurlMin {
            min_msat,
            balance_msat,
        });
    }
    let target_msat = balance_msat.min(max_value.milli_sats());
    let amount = InstructionAmount::from_milli_sats(target_msat).map_err(|_| {
        MaxSendableError::LnurlResolutionFailed(format!(
            "target amount {target_msat} msat exceeds Amount bounds"
        ))
    })?;
    let fixed = inst
        .clone()
        .set_amount(amount, resolver)
        .await
        .map_err(|e| MaxSendableError::LnurlResolutionFailed(e.to_string()))?;
    let bolt11 = fixed
        .methods()
        .iter()
        .find_map(|m| match m {
            PaymentMethod::LightningBolt11(inv) => Some(inv),
            _ => None,
        })
        .ok_or_else(|| {
            MaxSendableError::LnurlResolutionFailed(
                "LNURL resolution yielded no BOLT11 invoice".into(),
            )
        })?;
    Ok(bolt11_to_strategy(&bolt11.to_string()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use bitcoin_payment_instructions::hrn_resolution::DummyHrnResolver;
    use ldk_node::lightning::routing::router::{Path, RouteHop};
    use ldk_node::lightning::types::features::{ChannelFeatures, NodeFeatures};
    use std::str::FromStr;

    fn lsp() -> PublicKey {
        PublicKey::from_str("0279be667ef9dcbbac55a06295ce870b07029bfcdb2dce28d959f2815b16f81798")
            .unwrap()
    }

    fn other_peer() -> PublicKey {
        PublicKey::from_str("02c6047f9441ed7d6d3045406e95c07cd85c778e4b8cef3ca7abac09b95c709ee5")
            .unwrap()
    }

    fn snap(counterparty: PublicKey, is_usable: bool, limit_msat: u64) -> ChannelSnapshot {
        ChannelSnapshot {
            counterparty,
            is_usable,
            next_outbound_htlc_limit_msat: limit_msat,
        }
    }

    /// Run the public `compute_estimate` with `dest = None`. The
    /// closure panics if invoked — the None path must never route.
    /// Resolver is `DummyHrnResolver` because LNURL is unreachable
    /// with `dest = None`.
    async fn buffer_estimate(
        chans: &[ChannelSnapshot],
        lsp: &PublicKey,
        cfg: &MaxSendableConfig,
    ) -> Result<MaxSendableEstimate, MaxSendableError> {
        compute_estimate(None, chans, lsp, cfg, &DummyHrnResolver, |_| {
            panic!("None dest must not invoke find_route")
        })
        .await
    }

    /// Build a `Route` whose paths carry the given per-hop fee_msat
    /// values. `Path::fee_msat()` excludes the last hop (which
    /// carries the payment amount, not a fee), so each path needs
    /// at least one trailing "amount" hop.
    fn make_route(paths_fees: &[&[u64]]) -> Route {
        let hop = |fee_msat| RouteHop {
            pubkey: lsp(),
            node_features: NodeFeatures::empty(),
            short_channel_id: 0,
            channel_features: ChannelFeatures::empty(),
            fee_msat,
            cltv_expiry_delta: 0,
            maybe_announced_channel: false,
        };
        Route {
            paths: paths_fees
                .iter()
                .map(|hops| Path {
                    hops: hops.iter().map(|&f| hop(f)).collect(),
                    blinded_tail: None,
                })
                .collect(),
            route_params: None,
        }
    }

    #[tokio::test]
    async fn no_usable_channel_when_empty() {
        let res = buffer_estimate(&[], &lsp(), &MaxSendableConfig::default()).await;
        assert!(matches!(res, Err(MaxSendableError::NoUsableChannel)));
    }

    #[tokio::test]
    async fn no_usable_channel_when_only_other_counterparty() {
        let lsp = lsp();
        let chans = [snap(other_peer(), true, 100_000_000)];
        let res = buffer_estimate(&chans, &lsp, &MaxSendableConfig::default()).await;
        assert!(matches!(res, Err(MaxSendableError::NoUsableChannel)));
    }

    #[tokio::test]
    async fn no_usable_channel_when_lsp_channel_unusable() {
        // Mid-open/splice — distinct from "balance is zero".
        let lsp = lsp();
        let chans = [snap(lsp, false, 100_000_000)];
        let res = buffer_estimate(&chans, &lsp, &MaxSendableConfig::default()).await;
        assert!(matches!(res, Err(MaxSendableError::NoUsableChannel)));
    }

    #[tokio::test]
    async fn dust_balance_below_floor_returns_zero() {
        // 5 sats < 10-sat floor → buffer wins, amount saturates to 0.
        let lsp = lsp();
        let chans = [snap(lsp, true, 5_000)];
        let est = buffer_estimate(&chans, &lsp, &MaxSendableConfig::default())
            .await
            .unwrap();
        assert_eq!(est.amount_msat, 0);
        assert_eq!(est.fee_budget_msat, 10_000);
        assert_eq!(est.balance_at_compute_msat, 5_000);
    }

    #[tokio::test]
    async fn balance_exactly_equals_buffer_returns_zero() {
        let lsp = lsp();
        let chans = [snap(lsp, true, 10_000)];
        let est = buffer_estimate(&chans, &lsp, &MaxSendableConfig::default())
            .await
            .unwrap();
        assert_eq!(est.amount_msat, 0);
        assert_eq!(est.fee_budget_msat, 10_000);
    }

    #[tokio::test]
    async fn normal_case_percentage_buffer_dominates() {
        // 100k sats × 1% = 1000 sats > 10-sat floor → percentage wins.
        let lsp = lsp();
        let chans = [snap(lsp, true, 100_000_000)];
        let est = buffer_estimate(&chans, &lsp, &MaxSendableConfig::default())
            .await
            .unwrap();
        assert_eq!(est.fee_budget_msat, 1_000_000);
        assert_eq!(est.amount_msat, 99_000_000);
        assert_eq!(est.balance_at_compute_msat, 100_000_000);
    }

    #[tokio::test]
    async fn normal_case_floor_buffer_dominates() {
        // 500 sats × 1% = 5 sats < 10-sat floor → floor wins.
        let lsp = lsp();
        let chans = [snap(lsp, true, 500_000)];
        let est = buffer_estimate(&chans, &lsp, &MaxSendableConfig::default())
            .await
            .unwrap();
        assert_eq!(est.fee_budget_msat, 10_000);
        assert_eq!(est.amount_msat, 490_000);
    }

    #[tokio::test]
    async fn two_usable_lsp_channels_sum() {
        // No single-channel assumption: two usable LSP channels sum.
        let lsp = lsp();
        let chans = [snap(lsp, true, 50_000_000), snap(lsp, true, 30_000_000)];
        let est = buffer_estimate(&chans, &lsp, &MaxSendableConfig::default())
            .await
            .unwrap();
        assert_eq!(est.balance_at_compute_msat, 80_000_000);
        assert_eq!(est.fee_budget_msat, 800_000);
        assert_eq!(est.amount_msat, 79_200_000);
    }

    #[tokio::test]
    async fn mixed_channels_only_usable_lsp_contributes() {
        // Non-LSP and unusable-LSP entries are filtered out.
        let lsp = lsp();
        let other = other_peer();
        let chans = [
            snap(lsp, true, 10_000_000),
            snap(other, true, 50_000_000),
            snap(lsp, false, 100_000_000),
        ];
        let est = buffer_estimate(&chans, &lsp, &MaxSendableConfig::default())
            .await
            .unwrap();
        assert_eq!(est.balance_at_compute_msat, 10_000_000);
        assert_eq!(est.fee_budget_msat, 100_000);
        assert_eq!(est.amount_msat, 9_900_000);
    }

    #[tokio::test]
    async fn overrides_take_effect() {
        let lsp = lsp();
        let chans = [snap(lsp, true, 1_000_000_000)];
        let cfg = MaxSendableConfig {
            fee_buffer_bps: 200,
            fee_buffer_floor_sats: 50,
            ..MaxSendableConfig::default()
        };
        let est = buffer_estimate(&chans, &lsp, &cfg).await.unwrap();
        assert_eq!(est.fee_budget_msat, 20_000_000);
        assert_eq!(est.amount_msat, 980_000_000);
    }

    #[test]
    fn estimate_from_route_applies_default_multiplier() {
        // 3 hops, fees 50M + 30M, last hop = amount.
        // total_fees = 80_000_000, 1.1x multiplier = 88_000_000.
        let cfg = MaxSendableConfig::default();
        let route = make_route(&[&[50_000_000, 30_000_000, 1_000_000_000]]);
        let est = estimate_from_route(2_000_000_000, &route, &cfg);
        assert_eq!(est.fee_budget_msat, 88_000_000);
    }

    #[test]
    fn estimate_from_route_mpp_sums_across_paths() {
        // 2 paths × 2 hops. Fees per path = 1_000 and 2_000.
        // total_fees = 3_000, 1.1x multiplier = 3_300.
        let cfg = MaxSendableConfig::default();
        let route = make_route(&[&[1_000, 500_000], &[2_000, 500_000]]);
        let est = estimate_from_route(10_000_000, &route, &cfg);
        assert_eq!(est.fee_budget_msat, 3_300);
        assert_eq!(est.amount_msat, 9_996_700);
    }

    #[test]
    fn estimate_from_route_saturates_when_balance_below_fees() {
        // Fee budget exceeds balance — amount clamps to 0 rather
        // than underflowing.
        let cfg = MaxSendableConfig::default();
        let route = make_route(&[&[10_000, 1_000_000]]);
        let est = estimate_from_route(100, &route, &cfg);
        assert_eq!(est.amount_msat, 0);
        assert_eq!(est.balance_at_compute_msat, 100);
        assert!(est.fee_budget_msat > 100);
    }

    #[test]
    fn estimate_from_route_honours_multiplier_override() {
        // total_fees = 1_000_000. With multiplier=30_000 (3.0x):
        // fee_budget = 3_000_000.
        let cfg = MaxSendableConfig {
            route_retry_fee_multiplier_bps: 30_000,
            ..MaxSendableConfig::default()
        };
        let route = make_route(&[&[1_000_000, 100_000_000]]);
        let est = estimate_from_route(200_000_000, &route, &cfg);
        assert_eq!(est.fee_budget_msat, 3_000_000);
    }
}
