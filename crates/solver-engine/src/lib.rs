pub mod model;
pub mod policy;
pub mod routes;
use domain::{Amount, Candidate, OrderKind, Pool, Route, Solution, SolveRequest};
use policy::ProtocolPolicy;
use pool_math::amount_out;
use routes::{discover_routes, RoutePlan};
use std::cmp::Ordering;
use std::time::{SystemTime, UNIX_EPOCH};
pub const MAX_CANDIDATES: usize = 4;
const MAX_ORDERS: usize = 1;
const MAX_POOLS: usize = 64;
const MAX_SPLIT_RATIOS: usize = 2;
const MAX_ROUTES: usize = 2;
const MAX_TEXT: usize = 128;
pub const ONLINE_FEATURE_SCHEMA: [&str; 3] = ["expected_surplus", "gas_proxy", "route_count"];
pub const ONLINE_FEATURE_SCHEMA_HASH: &str =
    "96391eff9a684c3d02e7f1afc3b494ca5ae99aac628cad01e4bd3ac55f92eac5";

fn finite_u128(value: u128) -> Option<f64> {
    let converted = value as f64;
    converted.is_finite().then_some(converted)
}

pub fn rank_solution(
    solution: Solution,
    model: Option<&model::ModelArtifact>,
    min_validity: f64,
) -> Solution {
    rank_solution_unchecked(solution, model, min_validity)
}

pub fn rank_solution_for_request(
    mut solution: Solution,
    request: &SolveRequest,
    model: Option<&model::ModelArtifact>,
    min_validity: f64,
) -> Solution {
    let mut valid = Vec::with_capacity(solution.candidates.len());
    let mut rejected = false;
    for candidate in solution.candidates {
        if validate_candidate(request, &candidate) {
            valid.push(candidate);
        } else {
            rejected = true;
        }
    }
    solution.candidates = valid;
    if rejected {
        solution.reasons.push("invalid_generated_candidate".into());
        solution.reasons.sort();
        solution.reasons.dedup();
    }
    rank_solution_unchecked(solution, model, min_validity)
}

fn rank_solution_unchecked(
    mut solution: Solution,
    model: Option<&model::ModelArtifact>,
    min_validity: f64,
) -> Solution {
    let ranked = model.and_then(|m| {
        let mut predictions = Vec::with_capacity(solution.candidates.len());
        for candidate in &solution.candidates {
            let score = finite_u128(candidate.score)?;
            let surplus = finite_u128(candidate.expected_surplus.0)?;
            let features = [
                (ONLINE_FEATURE_SCHEMA[0], surplus),
                (ONLINE_FEATURE_SCHEMA[1], candidate.gas_proxy as f64),
                (ONLINE_FEATURE_SCHEMA[2], candidate.routes.len() as f64),
            ];
            let validity = m.predict_named(&features).ok()?.clamp(0.0, 1.0);
            let risk = validity * score;
            if !risk.is_finite() {
                return None;
            }
            predictions.push((validity, risk));
        }
        Some(predictions)
    });
    for (candidate, metadata) in solution
        .candidates
        .iter_mut()
        .zip(ranked.unwrap_or_default())
    {
        candidate.predicted_validity = Some(metadata.0);
        candidate.risk_value = Some(metadata.1);
        candidate.eligible = Some(metadata.0 >= min_validity);
    }
    if model.is_none()
        || solution
            .candidates
            .iter()
            .any(|c| c.predicted_validity.is_none())
    {
        for candidate in &mut solution.candidates {
            candidate.predicted_validity = None;
            candidate.risk_value = None;
            candidate.eligible = Some(true);
        }
    }
    solution.candidates.sort_by(|a, b| {
        b.eligible
            .cmp(&a.eligible)
            .then_with(|| {
                b.risk_value
                    .partial_cmp(&a.risk_value)
                    .unwrap_or(Ordering::Equal)
            })
            .then_with(|| b.score.cmp(&a.score))
            .then_with(|| a.id.cmp(&b.id))
    });
    solution.candidates.truncate(MAX_CANDIDATES);
    solution
}
const GAS_PROXY: u128 = 100;
pub const SOLVE_SAFETY_MARGIN_MS: u64 = 100;
pub fn solve(r: &SolveRequest) -> Solution {
    let n = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(u64::MAX, |d| d.as_millis().min(u64::MAX as u128) as u64);
    solve_at(r, n)
}
pub fn solve_at(r: &SolveRequest, now: u64) -> Solution {
    solve_at_with_policy(r, now, None)
}
pub fn solve_at_with_policy(
    r: &SolveRequest,
    now: u64,
    policy: Option<&ProtocolPolicy>,
) -> Solution {
    if let Some(p) = policy {
        let mut reasons = Vec::new();
        let o = r.auction.orders.first();
        if o.is_none_or(|o| {
            !p.allowed_tokens.contains(&o.sell_token) || !p.allowed_tokens.contains(&o.buy_token)
        }) {
            reasons.push("unsupported_token".into());
        }
        for pool in &r.pools {
            let Some(expected) = p.allowed_pools.iter().find(|x| x.id == pool.id) else {
                reasons.push("unsupported_pool".into());
                continue;
            };
            if expected.token0 != pool.token0
                || expected.token1 != pool.token1
                || expected.fee_bps != pool.fee_bps
                || expected.concentrated != pool.concentrated
            {
                reasons.push("protocol_pool_mismatch".into());
            }
        }
        if let Some(o) = o {
            if !p.allowed_pools.iter().any(|x| {
                (x.token0 == o.sell_token && x.token1 == o.buy_token)
                    || (x.token1 == o.sell_token && x.token0 == o.buy_token)
            }) {
                reasons.push("unsupported_token".into());
            }
        }
        reasons.sort();
        reasons.dedup();
        if !reasons.is_empty() {
            return empty(r, reasons);
        }
    }
    let mut solution = solve_at_unbounded(r, now);
    solution.candidates.truncate(MAX_CANDIDATES);
    solution
}
pub fn solve_at_unbounded(r: &SolveRequest, now: u64) -> Solution {
    let mut reasons = validate(r, now);
    if !reasons.is_empty() {
        return empty(r, reasons);
    };
    let o = &r.auction.orders[0];
    let e: Vec<_> = r
        .pools
        .iter()
        .filter_map(|p| direction(p, &o.sell_token, &o.buy_token).map(|(a, b)| (p, a, b)))
        .collect();
    let mut cs = vec![];
    for (p, a, b) in &e {
        if let Ok(out) = amount_out(o.sell_amount.0, *a, *b, p.fee_bps) {
            if out >= o.min_buy_amount.0 {
                if let Some(c) = candidate(r, p, o.sell_amount.0, out) {
                    if validate_candidate(r, &c) {
                        cs.push(c)
                    } else {
                        reasons.push("invalid_generated_candidate".into());
                    }
                }
            }
        }
    }
    if e.len() >= 2 && r.split_ratios.len() == 2 {
        let mut routes = vec![];
        let mut total: u128 = 0;
        let mut ok = true;
        let first_sell = o
            .sell_amount
            .0
            .checked_mul(r.split_ratios[0] as u128)
            .and_then(|x| x.checked_div(10000));
        let sells = first_sell.and_then(|first| {
            o.sell_amount
                .0
                .checked_sub(first)
                .map(|second| (first, second))
        });
        for (i, (p, a, b)) in e.iter().take(2).enumerate() {
            let s = sells.map(|(first, second)| if i == 0 { first } else { second });
            match s
                .filter(|x| *x > 0)
                .and_then(|x| amount_out(x, *a, *b, p.fee_bps).ok().map(|y| (x, y)))
            {
                Some((s, buy)) => {
                    total = match total.checked_add(buy) {
                        Some(x) => x,
                        None => {
                            ok = false;
                            0
                        }
                    };
                    routes.push(Route {
                        pool_id: p.id.clone(),
                        sell_amount: Amount(s),
                        buy_amount: Amount(buy),
                    })
                }
                None => ok = false,
            }
        }
        if ok && total >= o.min_buy_amount.0 {
            if let Some(score) = score(total, o.min_buy_amount.0, GAS_PROXY) {
                let c = Candidate {
                    id: format!("{}:split", r.auction.id),
                    auction_id: r.auction.id.clone(),
                    routes,
                    expected_surplus: Amount(total - o.min_buy_amount.0),
                    gas_proxy: GAS_PROXY as u64,
                    score,
                    predicted_validity: None,
                    risk_value: None,
                    eligible: None,
                };
                if validate_candidate(r, &c) {
                    cs.push(c)
                } else {
                    reasons.push("invalid_generated_candidate".into());
                }
            }
        }
    }
    // Bounded multi-hop discovery over allow-listed pools (max MAX_ROUTES hops).
    let plans = discover_routes(
        &r.pools,
        &o.sell_token,
        &o.buy_token,
        MAX_ROUTES,
        MAX_CANDIDATES,
    );
    for plan in plans {
        if plan.pools.len() < 2 {
            continue;
        }
        if let Some(c) = sequential_candidate(r, &plan, o.sell_amount.0, o.min_buy_amount.0) {
            if validate_candidate(r, &c) {
                cs.push(c);
            } else {
                reasons.push("invalid_generated_candidate".into());
            }
        }
    }
    cs.sort_by(|a, b| b.score.cmp(&a.score).then(a.id.cmp(&b.id)));
    reasons.sort();
    reasons.dedup();
    if cs.is_empty() {
        reasons.push("no_valid_candidate".into())
    };
    Solution {
        auction_id: r.auction.id.clone(),
        candidates: cs,
        reasons,
        provenance: None,
    }
}
pub fn validate_candidate(r: &SolveRequest, c: &Candidate) -> bool {
    let Some(o) = r.auction.orders.first() else {
        return false;
    };
    if c.auction_id != r.auction.id
        || c.id.is_empty()
        || c.id.len() > MAX_TEXT
        || c.routes.is_empty()
        || c.routes.len() > MAX_ROUTES
    {
        return false;
    }
    for route in &c.routes {
        if route.sell_amount.0 == 0
            || route.buy_amount.0 == 0
            || r.pools.iter().filter(|p| p.id == route.pool_id).count() != 1
            || c.routes
                .iter()
                .filter(|x| x.pool_id == route.pool_id)
                .count()
                != 1
        {
            return false;
        }
    }
    let parallel = c.routes.iter().all(|route| {
        r.pools
            .iter()
            .find(|p| p.id == route.pool_id)
            .and_then(|p| direction(p, &o.sell_token, &o.buy_token))
            .is_some()
    });
    let buy = if parallel {
        validate_parallel_candidate(r, c, o)
    } else {
        validate_sequential_candidate(r, c, o)
    };
    let Some(buy) = buy else {
        return false;
    };
    let expected_buy = match o.min_buy_amount.0.checked_add(c.expected_surplus.0) {
        Some(value) => value,
        None => return false,
    };
    let expected_score = match c.expected_surplus.0.checked_sub(c.gas_proxy as u128) {
        Some(value) if value > 0 => value,
        _ => return false,
    };
    buy == expected_buy && buy >= o.min_buy_amount.0 && c.score == expected_score
}

fn validate_parallel_candidate(r: &SolveRequest, c: &Candidate, o: &domain::Order) -> Option<u128> {
    let mut sell = 0u128;
    let mut buy = 0u128;
    for route in &c.routes {
        let pool = r.pools.iter().find(|p| p.id == route.pool_id)?;
        let (reserve_in, reserve_out) = direction(pool, &o.sell_token, &o.buy_token)?;
        if amount_out(route.sell_amount.0, reserve_in, reserve_out, pool.fee_bps)
            != Ok(route.buy_amount.0)
        {
            return None;
        }
        sell = sell.checked_add(route.sell_amount.0)?;
        buy = buy.checked_add(route.buy_amount.0)?;
    }
    (sell == o.sell_amount.0 && c.gas_proxy as u128 == GAS_PROXY).then_some(buy)
}

fn validate_sequential_candidate(
    r: &SolveRequest,
    c: &Candidate,
    o: &domain::Order,
) -> Option<u128> {
    let expected_gas = GAS_PROXY.checked_mul(c.routes.len() as u128)?;
    if c.gas_proxy as u128 != expected_gas {
        return None;
    }
    let mut token = o.sell_token.as_str();
    let mut previous_buy: Option<u128> = None;
    for (index, route) in c.routes.iter().enumerate() {
        let pool = r.pools.iter().find(|p| p.id == route.pool_id)?;
        if pool.concentrated {
            return None;
        }
        let (reserve_in, reserve_out, next) = if pool.token0 == token {
            (pool.reserve0.0, pool.reserve1.0, pool.token1.as_str())
        } else if pool.token1 == token {
            (pool.reserve1.0, pool.reserve0.0, pool.token0.as_str())
        } else {
            return None;
        };
        if amount_out(route.sell_amount.0, reserve_in, reserve_out, pool.fee_bps)
            != Ok(route.buy_amount.0)
        {
            return None;
        }
        if index == 0 {
            if route.sell_amount.0 != o.sell_amount.0 {
                return None;
            }
        } else if previous_buy != Some(route.sell_amount.0) {
            return None;
        }
        previous_buy = Some(route.buy_amount.0);
        token = next;
    }
    (token == o.buy_token).then_some(previous_buy?)
}

fn sequential_candidate(
    r: &SolveRequest,
    plan: &RoutePlan,
    sell: u128,
    min_buy: u128,
) -> Option<Candidate> {
    let mut routes = Vec::with_capacity(plan.pools.len());
    let mut amount = sell;
    for (index, pool_id) in plan.pools.iter().enumerate() {
        let pool = r.pools.iter().find(|p| &p.id == pool_id)?;
        let from = plan.tokens.get(index)?;
        let (reserve_in, reserve_out) = if pool.token0 == *from {
            (pool.reserve0.0, pool.reserve1.0)
        } else if pool.token1 == *from {
            (pool.reserve1.0, pool.reserve0.0)
        } else {
            return None;
        };
        let buy = amount_out(amount, reserve_in, reserve_out, pool.fee_bps).ok()?;
        routes.push(Route {
            pool_id: pool.id.clone(),
            sell_amount: Amount(amount),
            buy_amount: Amount(buy),
        });
        amount = buy;
    }
    if amount < min_buy {
        return None;
    }
    let gas = GAS_PROXY.checked_mul(routes.len() as u128)?;
    let score = score(amount, min_buy, gas)?;
    Some(Candidate {
        id: format!("{}:path:{}", r.auction.id, plan.pools.join(">")),
        auction_id: r.auction.id.clone(),
        routes,
        expected_surplus: Amount(amount - min_buy),
        gas_proxy: gas as u64,
        score,
        predicted_validity: None,
        risk_value: None,
        eligible: None,
    })
}

fn direction(p: &Pool, s: &str, b: &str) -> Option<(u128, u128)> {
    if p.token0 == s && p.token1 == b {
        Some((p.reserve0.0, p.reserve1.0))
    } else if p.token1 == s && p.token0 == b {
        Some((p.reserve1.0, p.reserve0.0))
    } else {
        None
    }
}
fn score(b: u128, m: u128, gas: u128) -> Option<u128> {
    let s = b.checked_sub(m)?;
    s.checked_sub(gas).filter(|_| s > gas)
}
fn candidate(r: &SolveRequest, p: &Pool, s: u128, b: u128) -> Option<Candidate> {
    Some(Candidate {
        id: format!("{}:{}:0", r.auction.id, p.id),
        auction_id: r.auction.id.clone(),
        routes: vec![Route {
            pool_id: p.id.clone(),
            sell_amount: Amount(s),
            buy_amount: Amount(b),
        }],
        expected_surplus: Amount(b.checked_sub(r.auction.orders[0].min_buy_amount.0)?),
        gas_proxy: GAS_PROXY as u64,
        score: score(b, r.auction.orders[0].min_buy_amount.0, GAS_PROXY)?,
        predicted_validity: None,
        risk_value: None,
        eligible: None,
    })
}
fn validate(r: &SolveRequest, n: u64) -> Vec<String> {
    let mut x = vec![];
    let a = &r.auction;
    if a.id.is_empty()
        || a.id.len() > MAX_TEXT
        || a.orders.len() != MAX_ORDERS
        || a.orders.len() > MAX_ORDERS
        || r.pools.len() > MAX_POOLS
        || r.split_ratios.len() > MAX_SPLIT_RATIOS
    {
        x.push("invalid_ids_or_order_count".into())
    }
    if r.pools
        .iter()
        .enumerate()
        .any(|(i, p)| r.pools[..i].iter().any(|previous| previous.id == p.id))
    {
        x.push("duplicate_pool_id".into());
    }
    if !a.tokens.is_empty()
        && (a.tokens.len() > MAX_POOLS
            || a.tokens.iter().any(|t| t.is_empty() || t.len() > MAX_TEXT)
            || a.tokens
                .iter()
                .enumerate()
                .any(|(i, t)| a.tokens[..i].iter().any(|previous| previous == t))
            || a.orders.first().is_none_or(|o| {
                !a.tokens.iter().any(|t| t == &o.sell_token)
                    || !a.tokens.iter().any(|t| t == &o.buy_token)
            }))
    {
        x.push("invalid_auction_tokens".into());
    }
    if a.block != a.state.observed_block {
        x.push("stale_state".into())
    }
    let d = match (r.deadline_ms, a.state.deadline_ms) {
        (0, d) => d,
        (d, 0) => d,
        (a, b) => a.min(b),
    };
    if d > 0 && n.checked_add(100).is_none_or(|t| t >= d) {
        x.push("deadline_exhausted".into())
    }
    if !r.split_ratios.is_empty()
        && (r.split_ratios.len() != 2
            || r.split_ratios.iter().any(|v| *v == 0 || *v >= 10000)
            || r.split_ratios
                .iter()
                .try_fold(0u32, |s, v| s.checked_add(*v))
                != Some(10000))
    {
        x.push("invalid_split_config".into())
    }
    for o in &a.orders {
        if o.id.is_empty()
            || o.id.len() > MAX_TEXT
            || o.sell_token.len() > MAX_TEXT
            || o.buy_token.len() > MAX_TEXT
        {
            x.push("invalid_ids_or_order_count".into())
        }
        if o.kind != OrderKind::Sell {
            x.push("unsupported_order_kind".into())
        }
        if o.sell_token == o.buy_token || o.sell_amount.0 == 0 || o.min_buy_amount.0 == 0 {
            x.push("invalid_order".into())
        }
    }
    for p in &r.pools {
        if p.id.is_empty()
            || p.id.len() > MAX_TEXT
            || p.token0.len() > MAX_TEXT
            || p.token1.len() > MAX_TEXT
            || p.concentrated
            || p.token0.is_empty()
            || p.token1.is_empty()
            || p.token0 == p.token1
            || p.reserve0.0 == 0
            || p.reserve1.0 == 0
            || p.fee_bps >= 10000
        {
            x.push("invalid_pool".into())
        }
    }
    x.sort();
    x.dedup();
    x
}
fn empty(r: &SolveRequest, mut x: Vec<String>) -> Solution {
    x.sort();
    x.dedup();
    Solution {
        auction_id: r.auction.id.clone(),
        candidates: vec![],
        reasons: x,
        provenance: None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use domain::{Auction, Order, OrderKind, State};

    fn request(deadline_ms: u64, concentrated: bool) -> SolveRequest {
        SolveRequest {
            schema_version: "1".into(),
            auction: Auction {
                id: "auction".into(),
                block: 1,
                tokens: vec![],
                orders: vec![Order {
                    id: "order".into(),
                    sell_token: "A".into(),
                    buy_token: "B".into(),
                    sell_amount: Amount(1_000),
                    min_buy_amount: Amount(900),
                    kind: OrderKind::Sell,
                }],
                state: State {
                    observed_block: 1,
                    deadline_ms: 0,
                },
            },
            pools: vec![Pool {
                id: "pool".into(),
                token0: "A".into(),
                token1: "B".into(),
                reserve0: Amount(1_000_000),
                reserve1: Amount(2_000_000),
                fee_bps: 30,
                concentrated,
            }],
            split_ratios: vec![],
            deadline_ms,
        }
    }

    #[test]
    fn successful_solve_accepts_future_deadline() {
        let solution = solve_at(&request(2_000, false), 1_000);
        assert!(!solution.candidates.is_empty());
        assert!(solution.reasons.is_empty());
    }

    #[test]
    fn expired_deadline_is_rejected_without_candidates() {
        let solution = solve_at(&request(1_000, false), 1_000);
        assert!(solution.candidates.is_empty());
        assert!(solution.reasons.contains(&"deadline_exhausted".to_string()));
    }

    #[test]
    fn concentrated_pool_is_rejected() {
        let solution = solve_at(&request(0, true), 1_000);
        assert!(solution.candidates.is_empty());
        assert!(solution.reasons.contains(&"invalid_pool".to_string()));
    }

    #[test]
    fn validate_candidate_requires_exact_pool_math_output() {
        let r = request(0, false);
        let mut c = solve_at(&r, 1_000).candidates[0].clone();
        assert!(validate_candidate(&r, &c));
        c.routes[0].buy_amount.0 += 1;
        c.expected_surplus.0 += 1;
        c.score += 1;
        assert!(!validate_candidate(&r, &c));
        c.routes[0].buy_amount.0 -= 2;
        c.expected_surplus.0 -= 2;
        c.score -= 2;
        assert!(!validate_candidate(&r, &c));
    }

    #[test]
    fn validate_candidate_rejects_math_failure_and_duplicate_pool_ids() {
        let mut r = request(0, false);
        let mut c = solve_at(&r, 1_000).candidates[0].clone();
        c.routes[0].buy_amount.0 = u128::MAX;
        assert!(!validate_candidate(&r, &c));
        r.pools.push(r.pools[0].clone());
        assert!(!validate_candidate(&r, &c));
    }

    #[test]
    fn request_aware_ranking_filters_invalid_candidates_and_records_reason() {
        let r = request(0, false);
        let valid = solve_at(&r, 1_000).candidates[0].clone();
        let mut invalid = valid.clone();
        invalid.id = "invalid".into();
        invalid.routes[0].buy_amount.0 += 1;
        let solution = rank_solution_for_request(
            Solution {
                auction_id: r.auction.id.clone(),
                candidates: vec![invalid, valid.clone()],
                reasons: vec![],
                provenance: None,
            },
            &r,
            None,
            0.0,
        );
        assert_eq!(solution.candidates.len(), 1);
        assert_eq!(solution.candidates[0].id, valid.id);
        assert!(solution
            .reasons
            .contains(&"invalid_generated_candidate".into()));
    }

    #[test]
    fn split_candidates_conserve_exact_pool_math() {
        let mut r = request(0, false);
        let mut second = r.pools[0].clone();
        second.id = "pool-2".into();
        second.reserve1 = Amount(1_500_000);
        r.pools.push(second);
        r.split_ratios = vec![5_000, 5_000];
        let solution = solve_at(&r, 1_000);
        assert!(solution.candidates.iter().any(|c| c.routes.len() == 2));
        assert!(solution
            .candidates
            .iter()
            .all(|c| validate_candidate(&r, c)));
    }

    #[test]
    fn split_candidates_use_exact_remainder_deterministically() {
        let mut r = request(0, false);
        r.auction.orders[0].sell_amount = Amount(1_001);
        let mut second = r.pools[0].clone();
        second.id = "pool-2".into();
        second.reserve1 = Amount(1_500_000);
        r.pools.push(second);
        r.split_ratios = vec![5_000, 5_000];
        let first = solve_at(&r, 1_000);
        let second = solve_at(&r, 1_000);
        let candidate = first
            .candidates
            .iter()
            .find(|c| c.routes.len() == 2)
            .unwrap();
        assert_eq!(candidate.routes[0].sell_amount, Amount(500));
        assert_eq!(candidate.routes[1].sell_amount, Amount(501));
        assert_eq!(
            candidate.routes[0].sell_amount.0 + candidate.routes[1].sell_amount.0,
            1_001
        );
        assert_eq!(first, second);
        assert!(validate_candidate(&r, candidate));
    }

    #[test]
    fn split_candidate_is_omitted_when_rounding_creates_zero_leg() {
        let mut r = request(0, false);
        r.auction.orders[0].sell_amount = Amount(1);
        let mut second = r.pools[0].clone();
        second.id = "pool-2".into();
        r.pools.push(second);
        r.split_ratios = vec![5_000, 5_000];
        let solution = solve_at(&r, 1_000);
        assert!(solution.candidates.iter().all(|c| c.routes.len() != 2));
    }

    fn weth_usdc_policy() -> ProtocolPolicy {
        ProtocolPolicy::parse(
            br#"{
                "network":"ethereum-mainnet",
                "allowed_tokens":["WETH","USDC"],
                "allowed_pools":[{
                    "id":"pool",
                    "token0":"WETH",
                    "token1":"USDC",
                    "fee_bps":30,
                    "concentrated":false
                }]
            }"#,
        )
        .unwrap()
    }

    #[test]
    fn multi_hop_candidates_are_discovered_and_validate() {
        let mut r = request(0, false);
        r.auction.orders[0].sell_token = "A".into();
        r.auction.orders[0].buy_token = "C".into();
        r.auction.orders[0].min_buy_amount = Amount(100);
        r.pools = vec![
            Pool {
                id: "ab".into(),
                token0: "A".into(),
                token1: "B".into(),
                reserve0: Amount(1_000_000),
                reserve1: Amount(1_000_000),
                fee_bps: 30,
                concentrated: false,
            },
            Pool {
                id: "bc".into(),
                token0: "B".into(),
                token1: "C".into(),
                reserve0: Amount(1_000_000),
                reserve1: Amount(1_000_000),
                fee_bps: 30,
                concentrated: false,
            },
        ];
        let solution = solve_at(&r, 1_000);
        let multi = solution
            .candidates
            .iter()
            .find(|c| c.routes.len() == 2)
            .expect("multi-hop candidate");
        assert_eq!(multi.gas_proxy as u128, GAS_PROXY * 2);
        assert!(validate_candidate(&r, multi));
        assert!(solution
            .candidates
            .iter()
            .all(|c| validate_candidate(&r, c)));
    }

    #[test]
    fn policy_accepts_both_order_directions_but_rejects_pool_reorientation() {
        let policy = weth_usdc_policy();
        let mut forward = request(0, false);
        forward.auction.orders[0].sell_token = "WETH".into();
        forward.auction.orders[0].buy_token = "USDC".into();
        forward.pools[0].token0 = "WETH".into();
        forward.pools[0].token1 = "USDC".into();
        let solution = solve_at_with_policy(&forward, 1_000, Some(&policy));
        assert!(!solution.candidates.is_empty());
        assert!(solution.reasons.is_empty());

        let mut reverse = forward.clone();
        reverse.auction.orders[0].sell_token = "USDC".into();
        reverse.auction.orders[0].buy_token = "WETH".into();
        reverse.auction.orders[0].min_buy_amount = Amount(300);
        let solution = solve_at_with_policy(&reverse, 1_000, Some(&policy));
        assert!(!solution.candidates.is_empty());
        assert!(solution.reasons.is_empty());

        reverse.pools[0].token0 = "USDC".into();
        reverse.pools[0].token1 = "WETH".into();
        let solution = solve_at_with_policy(&reverse, 1_000, Some(&policy));
        assert!(solution.candidates.is_empty());
        assert!(solution
            .reasons
            .contains(&"protocol_pool_mismatch".to_string()));
    }
}
