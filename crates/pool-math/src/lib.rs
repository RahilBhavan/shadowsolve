#[derive(Debug, PartialEq, Eq)]
pub enum MathError {
    ZeroInput,
    InsufficientLiquidity,
    InvalidFee,
    ArithmeticOverflow,
}

/// Constant-product swap; concentrated liquidity is intentionally unsupported.
pub fn amount_out(
    amount_in: u128,
    reserve_in: u128,
    reserve_out: u128,
    fee_bps: u32,
) -> Result<u128, MathError> {
    if amount_in == 0 {
        return Err(MathError::ZeroInput);
    }
    if reserve_in == 0 || reserve_out == 0 {
        return Err(MathError::InsufficientLiquidity);
    }
    if fee_bps >= 10_000 {
        return Err(MathError::InvalidFee);
    }
    let scale = 10_000u128;
    let fee_multiplier = scale - fee_bps as u128;
    let net = amount_in
        .checked_mul(fee_multiplier)
        .ok_or(MathError::ArithmeticOverflow)?
        / scale;
    let denominator = reserve_in
        .checked_add(net)
        .ok_or(MathError::ArithmeticOverflow)?;
    let out = reserve_out
        .checked_mul(net)
        .ok_or(MathError::ArithmeticOverflow)?
        / denominator;
    if out >= reserve_out {
        return Err(MathError::InsufficientLiquidity);
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn exact_fee_free() {
        assert_eq!(amount_out(100, 1000, 1000, 0), Ok(90));
    }
    #[test]
    fn exact_fee() {
        assert_eq!(amount_out(100, 1000, 1000, 30), Ok(90));
    }
    #[test]
    fn rejects() {
        assert_eq!(amount_out(0, 1, 1, 0), Err(MathError::ZeroInput));
        assert_eq!(
            amount_out(1, 0, 1, 0),
            Err(MathError::InsufficientLiquidity)
        );
    }
    #[test]
    fn rejects_multiplication_overflow() {
        assert_eq!(
            amount_out(u128::MAX, 1, 1, 0),
            Err(MathError::ArithmeticOverflow)
        );
    }
    #[test]
    fn rejects_addition_overflow() {
        assert_eq!(
            amount_out(u128::MAX, u128::MAX, 1, 9_999),
            Err(MathError::ArithmeticOverflow)
        );
    }
}
