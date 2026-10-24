use domain::Pool;
use pool_math::{amount_out, MathError};
use std::collections::BTreeSet;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RoutePlan {
    pub pools: Vec<String>,
    pub tokens: Vec<String>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct GasConfig {
    pub units_per_hop: u64,
    pub gas_price_wei: u128,
    pub native_to_output_num: u128,
    pub native_to_output_den: u128,
}
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RouteQuote {
    pub gross_output: u128,
    pub gas_cost: u128,
    pub net_surplus: i128,
}

pub fn quote_route(
    route: &RoutePlan,
    pools: &[Pool],
    amount: u128,
    gas: &GasConfig,
) -> Result<RouteQuote, MathError> {
    if gas.native_to_output_den == 0
        || route.pools.is_empty()
        || route.tokens.len() != route.pools.len() + 1
    {
        return Err(MathError::ArithmeticOverflow);
    }
    let gross_output = simulate_route(route, pools, amount)?;
    let gas_cost = (gas.units_per_hop as u128)
        .checked_mul(route.pools.len() as u128)
        .and_then(|u| u.checked_mul(gas.gas_price_wei))
        .and_then(|w| w.checked_mul(gas.native_to_output_num))
        .and_then(|v| v.checked_div(gas.native_to_output_den))
        .ok_or(MathError::ArithmeticOverflow)?;
    Ok(RouteQuote {
        gross_output,
        gas_cost,
        net_surplus: i128::try_from(gross_output)
            .and_then(|gross| i128::try_from(gas_cost).map(|gas| gross.checked_sub(gas)))
            .ok()
            .flatten()
            .ok_or(MathError::ArithmeticOverflow)?,
    })
}

pub fn discover_routes(
    pools: &[Pool],
    sell: &str,
    buy: &str,
    max_hops: usize,
    max_routes: usize,
) -> Vec<RoutePlan> {
    let mut out = Vec::new();
    let mut path = Vec::new();
    let mut tokens = vec![sell.to_string()];
    let mut used = BTreeSet::new();
    #[allow(clippy::too_many_arguments)]
    fn walk(
        pools: &[Pool],
        current: &str,
        buy: &str,
        max: usize,
        limit: usize,
        path: &mut Vec<String>,
        tokens: &mut Vec<String>,
        used: &mut BTreeSet<String>,
        out: &mut Vec<RoutePlan>,
    ) {
        if out.len() >= limit {
            return;
        }
        if current == buy && !path.is_empty() {
            out.push(RoutePlan {
                pools: path.clone(),
                tokens: tokens.clone(),
            });
            return;
        }
        if path.len() >= max {
            return;
        }
        for p in pools {
            if p.concentrated || used.contains(&p.id) {
                continue;
            }
            let next = if p.token0 == current {
                &p.token1
            } else if p.token1 == current {
                &p.token0
            } else {
                continue;
            };
            if tokens.iter().any(|t| t == next) {
                continue;
            }
            used.insert(p.id.clone());
            path.push(p.id.clone());
            tokens.push(next.clone());
            walk(pools, next, buy, max, limit, path, tokens, used, out);
            tokens.pop();
            path.pop();
            used.remove(&p.id);
        }
    }
    if max_hops > 0 && max_routes > 0 {
        walk(
            pools,
            sell,
            buy,
            max_hops,
            max_routes,
            &mut path,
            &mut tokens,
            &mut used,
            &mut out,
        );
    }
    out.sort_by(|a, b| a.pools.cmp(&b.pools));
    out.dedup_by(|a, b| a.pools == b.pools);
    out.truncate(max_routes);
    out
}

pub fn simulate_route(route: &RoutePlan, pools: &[Pool], amount: u128) -> Result<u128, MathError> {
    if route.pools.is_empty() || route.tokens.len() != route.pools.len() + 1 {
        return Err(MathError::InsufficientLiquidity);
    }
    let mut value = amount;
    for (index, id) in route.pools.iter().enumerate() {
        let p = pools
            .iter()
            .find(|p| &p.id == id)
            .ok_or(MathError::InsufficientLiquidity)?;
        if p.concentrated {
            return Err(MathError::InsufficientLiquidity);
        }
        let from = &route.tokens[index];
        let (a, b) = if p.token0 == *from {
            (p.reserve0.0, p.reserve1.0)
        } else if p.token1 == *from {
            (p.reserve1.0, p.reserve0.0)
        } else {
            return Err(MathError::InsufficientLiquidity);
        };
        value = amount_out(value, a, b, p.fee_bps)?;
    }
    Ok(value)
}

#[cfg(test)]
mod tests {
    use super::*;
    use domain::Amount;

    fn pool(id: &str, a: &str, b: &str) -> Pool {
        Pool {
            id: id.into(),
            token0: a.into(),
            token1: b.into(),
            reserve0: Amount(1_000_000),
            reserve1: Amount(1_000_000),
            fee_bps: 30,
            concentrated: false,
        }
    }

    #[test]
    fn routes_are_bounded_and_cycle_free() {
        let pools = vec![
            pool("ab", "A", "B"),
            pool("bc", "B", "C"),
            pool("ca", "C", "A"),
        ];
        let routes = discover_routes(&pools, "A", "C", 2, 4);
        assert_eq!(routes.len(), 2);
        assert!(routes.iter().any(|r| r.pools == vec!["ab", "bc"]));
    }

    #[test]
    fn simulation_composes_integer_math_and_rejects_concentrated() {
        let mut pools = vec![pool("ab", "A", "B"), pool("bc", "B", "C")];
        let route = discover_routes(&pools, "A", "C", 2, 1).pop().unwrap();
        assert!(simulate_route(&route, &pools, 1_000).unwrap() > 0);
        pools[1].concentrated = true;
        assert!(discover_routes(&pools, "A", "C", 2, 1).is_empty());
    }
}
