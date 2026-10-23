//! Shared fixed-point decimal arithmetic for the Swap Engine.
//!
//! Every quoting, fee, allocation and fill computation in [`crate::swap`]
//! goes through [`Decimal`]. Native floating point (`f32`/`f64`) is never
//! used for money: a regression test in this module fails the build if a
//! float type appears anywhere under `src/swap/`.
//!
//! # Representation
//!
//! A [`Decimal`] is an arbitrary-precision integer number of *units* plus a
//! `scale` (number of fractional digits): `value = units / 10^scale`. All
//! arithmetic is exact integer arithmetic. The only places precision can be
//! lost are the operations that take an explicit [`RoundingMode`] —
//! [`Decimal::rescale`], [`Decimal::mul`], [`Decimal::div`] and
//! [`Decimal::mul_bps`] — and callers pick that mode from the shared
//! [`RoundingPolicy`](crate::swap::rounding::RoundingPolicy) rather than
//! choosing ad hoc.
//!
//! # Wire format
//!
//! Amounts and rates cross every boundary (HTTP, storage, logs) as **JSON
//! strings**, never JSON numbers:
//!
//! * grammar: `-?[0-9]+(\.[0-9]+)?` — no exponent, no leading `+`, no bare
//!   `.5`, no thousands separators, at most [`MAX_INPUT_LEN`] characters;
//! * at most [`MAX_SCALE`] fractional digits;
//! * serialization always prints exactly `scale` fractional digits, so
//!   `"1.5000000"` round-trips with its precision intact.
//!
//! Deserializing a JSON number is rejected on purpose: by the time a client
//! JSON parser has produced `0.1` it may already have been through a float.
//!
//! # Precision
//!
//! * Asset amounts carry the asset's own precision
//!   ([`Asset::decimals`](crate::swap::asset::Asset), 7 for Stellar assets).
//! * Exchange rates carry [`RATE_SCALE`] (18) fractional digits.
//! * Basis-point figures (price impact, slippage) carry [`BPS_SCALE`] (2).

use num_bigint::{BigInt, Sign};
use num_integer::Integer;
use num_traits::{Signed, ToPrimitive, Zero};
use serde::{Deserialize, Deserializer, Serialize, Serializer};
use sqlx::types::BigDecimal;
use std::cmp::Ordering;
use std::fmt;
use std::str::FromStr;

pub use crate::swap::rounding::RoundingMode;

/// Maximum number of fractional digits a [`Decimal`] may carry.
pub const MAX_SCALE: u32 = 36;
/// Fractional digits used for exchange rates.
pub const RATE_SCALE: u32 = 18;
/// Fractional digits used for basis-point figures (e.g. `"12.34"` bps).
pub const BPS_SCALE: u32 = 2;
/// One whole in basis points.
pub const BPS_DENOMINATOR: u32 = 10_000;
/// Longest decimal string accepted by the parser.
pub const MAX_INPUT_LEN: usize = 80;

/// Errors produced by decimal parsing and arithmetic.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum DecimalError {
    #[error("invalid decimal string {0:?}: expected digits with an optional '.' fraction")]
    Invalid(String),
    #[error("decimal string longer than {MAX_INPUT_LEN} characters")]
    TooLong,
    #[error("{digits} fractional digits exceeds the allowed precision of {max}")]
    ExcessPrecision { digits: u32, max: u32 },
    #[error("division by zero")]
    DivisionByZero,
    #[error("amounts must be encoded as JSON strings, not numbers")]
    NotAString,
}

/// Exact fixed-point decimal: `units / 10^scale`.
#[derive(Clone)]
pub struct Decimal {
    units: BigInt,
    scale: u32,
}

fn pow10(exp: u32) -> BigInt {
    num_traits::pow(BigInt::from(10u8), exp as usize)
}

/// Divide `num / den` and round the quotient to an integer using `mode`.
/// This is the single rounding primitive every other operation uses.
pub(crate) fn round_div(num: &BigInt, den: &BigInt, mode: RoundingMode) -> BigInt {
    assert!(!den.is_zero(), "round_div called with zero denominator");
    let (q, r) = num.div_rem(den); // truncated toward zero
    if r.is_zero() {
        return q;
    }
    let negative = (num.sign() == Sign::Minus) != (den.sign() == Sign::Minus);
    // One step away from zero.
    let away = if negative { &q - 1 } else { &q + 1 };
    let twice_r: BigInt = Signed::abs(&r) * 2u8;
    let abs_den: BigInt = Signed::abs(den);
    match mode {
        RoundingMode::Down => q,
        RoundingMode::Up => away,
        RoundingMode::Floor => {
            if negative {
                away
            } else {
                q
            }
        }
        RoundingMode::Ceiling => {
            if negative {
                q
            } else {
                away
            }
        }
        RoundingMode::HalfUp => {
            if twice_r >= abs_den {
                away
            } else {
                q
            }
        }
        RoundingMode::HalfDown => {
            if twice_r > abs_den {
                away
            } else {
                q
            }
        }
        RoundingMode::HalfEven => match twice_r.cmp(&abs_den) {
            Ordering::Greater => away,
            Ordering::Less => q,
            Ordering::Equal => {
                if q.is_odd() {
                    away
                } else {
                    q
                }
            }
        },
    }
}

impl Decimal {
    /// Zero at the given scale.
    pub fn zero(scale: u32) -> Self {
        Self::from_units(BigInt::zero(), scale)
    }

    /// Build from an integer number of units at `scale`
    /// (`from_units(15, 1)` is `1.5`).
    pub fn from_units(units: impl Into<BigInt>, scale: u32) -> Self {
        assert!(scale <= MAX_SCALE, "scale {scale} exceeds MAX_SCALE");
        Self {
            units: units.into(),
            scale,
        }
    }

    /// A whole number at scale 0.
    pub fn from_int(value: i64) -> Self {
        Self::from_units(value, 0)
    }

    /// Parse a wire-format string, keeping exactly the precision given
    /// (`"1.50"` has scale 2).
    pub fn parse(s: &str) -> Result<Self, DecimalError> {
        if s.len() > MAX_INPUT_LEN {
            return Err(DecimalError::TooLong);
        }
        let (negative, body) = match s.strip_prefix('-') {
            Some(rest) => (true, rest),
            None => (false, s),
        };
        let (int_part, frac_part) = match body.split_once('.') {
            Some((i, f)) => (i, f),
            None => (body, ""),
        };
        let digits_ok = |p: &str| !p.is_empty() && p.bytes().all(|b| b.is_ascii_digit());
        if !digits_ok(int_part) || (body.contains('.') && !digits_ok(frac_part)) {
            return Err(DecimalError::Invalid(s.to_string()));
        }
        let scale = frac_part.len() as u32;
        if scale > MAX_SCALE {
            return Err(DecimalError::ExcessPrecision {
                digits: scale,
                max: MAX_SCALE,
            });
        }
        let mut units = BigInt::from_str(&format!("{int_part}{frac_part}"))
            .map_err(|_| DecimalError::Invalid(s.to_string()))?;
        if negative {
            units = -units;
        }
        Ok(Self { units, scale })
    }

    /// Parse and normalise to exactly `scale` digits. Input with *more*
    /// fractional digits than `scale` is rejected rather than silently
    /// rounded — a client asking to swap `1.00000001` of a 7-decimal asset
    /// gets an error, not a different amount.
    pub fn parse_with_scale(s: &str, scale: u32) -> Result<Self, DecimalError> {
        let d = Self::parse(s)?;
        if d.scale > scale {
            return Err(DecimalError::ExcessPrecision {
                digits: d.scale,
                max: scale,
            });
        }
        Ok(d.rescale(scale, RoundingMode::Down)) // widening: exact
    }

    pub fn scale(&self) -> u32 {
        self.scale
    }

    /// Integer number of `10^-scale` units.
    pub fn units(&self) -> &BigInt {
        &self.units
    }

    pub fn is_zero(&self) -> bool {
        self.units.is_zero()
    }

    pub fn is_positive(&self) -> bool {
        self.units.is_positive()
    }

    pub fn is_negative(&self) -> bool {
        self.units.is_negative()
    }

    pub fn abs(&self) -> Self {
        Self::from_units(self.units.abs(), self.scale)
    }

    /// Change scale. Widening is exact; narrowing rounds with `mode`.
    pub fn rescale(&self, scale: u32, mode: RoundingMode) -> Self {
        assert!(scale <= MAX_SCALE, "scale {scale} exceeds MAX_SCALE");
        match scale.cmp(&self.scale) {
            Ordering::Equal => self.clone(),
            Ordering::Greater => Self::from_units(&self.units * pow10(scale - self.scale), scale),
            Ordering::Less => Self::from_units(
                round_div(&self.units, &pow10(self.scale - scale), mode),
                scale,
            ),
        }
    }

    /// The units of `self` and `other` expressed at a common scale.
    fn aligned(&self, other: &Self) -> (BigInt, BigInt, u32) {
        let scale = self.scale.max(other.scale);
        (
            self.rescale(scale, RoundingMode::Down).units,
            other.rescale(scale, RoundingMode::Down).units,
            scale,
        )
    }

    /// Exact sum; result scale is the larger of the two.
    pub fn add(&self, other: &Self) -> Self {
        let (a, b, scale) = self.aligned(other);
        Self::from_units(a + b, scale)
    }

    /// Exact difference; result scale is the larger of the two.
    pub fn sub(&self, other: &Self) -> Self {
        let (a, b, scale) = self.aligned(other);
        Self::from_units(a - b, scale)
    }

    /// `self * other`, rounded to `scale` with `mode`.
    pub fn mul(&self, other: &Self, scale: u32, mode: RoundingMode) -> Self {
        let exact = Self {
            units: &self.units * &other.units,
            scale: self.scale + other.scale,
        };
        if exact.scale <= MAX_SCALE {
            exact.rescale(scale, mode)
        } else {
            // Exact product is finer than MAX_SCALE; round straight to target.
            let den = pow10(exact.scale - scale);
            Self::from_units(round_div(&exact.units, &den, mode), scale)
        }
    }

    /// `self / other`, rounded to `scale` with `mode`. Exact integer
    /// division — no intermediate precision limit.
    pub fn div(&self, other: &Self, scale: u32, mode: RoundingMode) -> Result<Self, DecimalError> {
        if other.is_zero() {
            return Err(DecimalError::DivisionByZero);
        }
        // value = (a / 10^sa) / (b / 10^sb); want units at `scale`:
        // a * 10^(scale + sb - sa) / b
        let shift = scale as i64 + other.scale as i64 - self.scale as i64;
        let (num, den) = if shift >= 0 {
            (&self.units * pow10(shift as u32), other.units.clone())
        } else {
            (self.units.clone(), &other.units * pow10((-shift) as u32))
        };
        Ok(Self::from_units(round_div(&num, &den, mode), scale))
    }

    /// `self * bps / 10_000`, rounded to `scale` with `mode`.
    pub fn mul_bps(&self, bps: u32, scale: u32, mode: RoundingMode) -> Self {
        let num = &self.units * BigInt::from(bps) * pow10(scale);
        let den = BigInt::from(BPS_DENOMINATOR) * pow10(self.scale);
        Self::from_units(round_div(&num, &den, mode), scale)
    }

    /// Convert to `BigDecimal` (for sqlx / the Phase 1 hand-off).
    pub fn to_bigdecimal(&self) -> BigDecimal {
        BigDecimal::new(self.units.clone(), self.scale as i64)
    }

    /// Convert from `BigDecimal`. Values finer than [`MAX_SCALE`] are
    /// rejected rather than rounded.
    pub fn from_bigdecimal(value: &BigDecimal) -> Result<Self, DecimalError> {
        let (units, exponent) = value.as_bigint_and_exponent();
        if exponent < 0 {
            let factor = pow10(exponent.unsigned_abs() as u32);
            return Ok(Self::from_units(units * factor, 0));
        }
        if exponent > MAX_SCALE as i64 {
            return Err(DecimalError::ExcessPrecision {
                digits: exponent as u32,
                max: MAX_SCALE,
            });
        }
        Ok(Self::from_units(units, exponent as u32))
    }

    /// Units as `i128`, when they fit (handy for tests and metrics labels).
    pub fn units_i128(&self) -> Option<i128> {
        self.units.to_i128()
    }
}

impl fmt::Display for Decimal {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let digits = self.units.abs().to_string();
        let sign = if self.units.is_negative() { "-" } else { "" };
        let scale = self.scale as usize;
        if scale == 0 {
            return write!(f, "{sign}{digits}");
        }
        let padded = format!("{digits:0>width$}", width = scale + 1);
        let (int_part, frac_part) = padded.split_at(padded.len() - scale);
        write!(f, "{sign}{int_part}.{frac_part}")
    }
}

impl fmt::Debug for Decimal {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "Decimal({self})")
    }
}

impl FromStr for Decimal {
    type Err = DecimalError;
    fn from_str(s: &str) -> Result<Self, Self::Err> {
        Self::parse(s)
    }
}

/// Numeric equality: `1.5 == 1.50`.
impl PartialEq for Decimal {
    fn eq(&self, other: &Self) -> bool {
        self.cmp(other) == Ordering::Equal
    }
}

impl Eq for Decimal {}

impl PartialOrd for Decimal {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}

impl Ord for Decimal {
    fn cmp(&self, other: &Self) -> Ordering {
        let (a, b, _) = self.aligned(other);
        a.cmp(&b)
    }
}

impl Serialize for Decimal {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.serialize_str(&self.to_string())
    }
}

impl<'de> Deserialize<'de> for Decimal {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        struct Visitor;
        impl<'de> serde::de::Visitor<'de> for Visitor {
            type Value = Decimal;
            fn expecting(&self, f: &mut fmt::Formatter) -> fmt::Result {
                f.write_str("a decimal amount encoded as a string, e.g. \"100.50\"")
            }
            fn visit_str<E: serde::de::Error>(self, v: &str) -> Result<Decimal, E> {
                Decimal::parse(v).map_err(E::custom)
            }
            fn visit_i64<E: serde::de::Error>(self, _: i64) -> Result<Decimal, E> {
                Err(E::custom(DecimalError::NotAString))
            }
            fn visit_u64<E: serde::de::Error>(self, _: u64) -> Result<Decimal, E> {
                Err(E::custom(DecimalError::NotAString))
            }
            fn visit_f64<E: serde::de::Error>(self, _: f64) -> Result<Decimal, E> {
                Err(E::custom(DecimalError::NotAString))
            }
        }
        deserializer.deserialize_any(Visitor)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use RoundingMode::*;

    fn d(s: &str) -> Decimal {
        Decimal::parse(s).unwrap()
    }

    #[test]
    fn parse_and_display_round_trip_preserving_scale() {
        for s in [
            "0",
            "1",
            "-1",
            "1.50",
            "0.0000001",
            "-0.05",
            "123456789012345678901234567890.123",
        ] {
            assert_eq!(d(s).to_string(), s);
        }
        assert_eq!(d("1.50").scale(), 2);
        assert_eq!(d("007.10").to_string(), "7.10");
    }

    #[test]
    fn parse_rejects_non_canonical_forms() {
        for s in [
            "", "-", ".5", "5.", "1e5", "+1", "1,000", " 1", "1.2.3", "NaN", "inf", "0x10",
        ] {
            assert!(Decimal::parse(s).is_err(), "{s:?} should be rejected");
        }
        assert_eq!(Decimal::parse(&"1".repeat(81)), Err(DecimalError::TooLong));
        let too_fine = format!("0.{}", "1".repeat(37));
        assert!(matches!(
            Decimal::parse(&too_fine),
            Err(DecimalError::ExcessPrecision { .. })
        ));
    }

    #[test]
    fn parse_with_scale_pads_but_never_rounds() {
        assert_eq!(
            Decimal::parse_with_scale("1.5", 7).unwrap().to_string(),
            "1.5000000"
        );
        assert_eq!(
            Decimal::parse_with_scale("1.00000001", 7),
            Err(DecimalError::ExcessPrecision { digits: 8, max: 7 })
        );
    }

    #[test]
    fn equality_and_ordering_are_numeric() {
        assert_eq!(d("1.5"), d("1.50000"));
        assert!(d("1.05") < d("1.5"));
        assert!(d("-2") < d("-1.999"));
        assert_eq!(d("2").max(d("1.99")), d("2"));
    }

    #[test]
    fn rounding_modes_match_their_definitions() {
        // value, then expected at scale 0 for Down, Up, Floor, Ceiling, HalfUp, HalfDown, HalfEven
        let table = [
            ("2.5", ["2", "3", "2", "3", "3", "2", "2"]),
            ("3.5", ["3", "4", "3", "4", "4", "3", "4"]),
            ("2.4", ["2", "3", "2", "3", "2", "2", "2"]),
            ("2.6", ["2", "3", "2", "3", "3", "3", "3"]),
            ("-2.5", ["-2", "-3", "-3", "-2", "-3", "-2", "-2"]),
            ("-2.6", ["-2", "-3", "-3", "-2", "-3", "-3", "-3"]),
            ("2.0", ["2", "2", "2", "2", "2", "2", "2"]),
        ];
        let modes = [Down, Up, Floor, Ceiling, HalfUp, HalfDown, HalfEven];
        for (input, expected) in table {
            for (mode, want) in modes.iter().zip(expected) {
                assert_eq!(
                    d(input).rescale(0, *mode).to_string(),
                    want,
                    "{input} {mode:?}"
                );
            }
        }
    }

    #[test]
    fn add_and_sub_are_exact_across_scales() {
        assert_eq!(d("0.1").add(&d("0.2")).to_string(), "0.3");
        assert_eq!(d("1").sub(&d("0.0000001")).to_string(), "0.9999999");
        assert_eq!(d("0.10").add(&d("0.005")).scale(), 3);
    }

    #[test]
    fn mul_and_div_round_once_at_the_end() {
        assert_eq!(
            d("1.2345678").mul(&d("0.5"), 7, Down).to_string(),
            "0.6172839"
        );
        assert_eq!(
            d("1.2345678").mul(&d("0.5"), 7, Up).to_string(),
            "0.6172839"
        );
        assert_eq!(
            d("1.2345679").mul(&d("0.5"), 7, Up).to_string(),
            "0.6172840"
        );
        assert_eq!(
            d("1").div(&d("3"), 7, Down).unwrap().to_string(),
            "0.3333333"
        );
        assert_eq!(
            d("2").div(&d("3"), 7, HalfEven).unwrap().to_string(),
            "0.6666667"
        );
        assert_eq!(
            d("1").div(&d("0"), 7, Down),
            Err(DecimalError::DivisionByZero)
        );
        // negative shift path: dividend finer than target
        assert_eq!(
            d("0.000000000123").div(&d("2"), 3, Up).unwrap().to_string(),
            "0.001"
        );
    }

    #[test]
    fn mul_beyond_max_scale_rounds_directly() {
        let a = Decimal::from_units(1, 30);
        let b = Decimal::from_units(15, 30);
        assert_eq!(
            a.mul(&b, 36, Down).to_string(),
            format!("0.{}", "0".repeat(36))
        );
        let r = d("1.000000000000000001").mul(&d("1.000000000000000001"), 18, HalfEven);
        assert_eq!(r.to_string(), "1.000000000000000002");
    }

    #[test]
    fn mul_bps() {
        assert_eq!(d("100.0000000").mul_bps(30, 7, Up).to_string(), "0.3000000");
        assert_eq!(d("0.0000001").mul_bps(30, 7, Up).to_string(), "0.0000001");
        assert_eq!(d("0.0000001").mul_bps(30, 7, Down).to_string(), "0.0000000");
    }

    #[test]
    fn bigdecimal_round_trip() {
        let original = d("1234.5678900");
        let back = Decimal::from_bigdecimal(&original.to_bigdecimal()).unwrap();
        assert_eq!(back.to_string(), "1234.5678900");
        let big = BigDecimal::from_str("1e3").unwrap();
        assert_eq!(Decimal::from_bigdecimal(&big).unwrap().to_string(), "1000");
    }

    #[test]
    fn serde_uses_strings_and_rejects_json_numbers() {
        let json = serde_json::to_string(&d("1.5000000")).unwrap();
        assert_eq!(json, "\"1.5000000\"");
        let back: Decimal = serde_json::from_str(&json).unwrap();
        assert_eq!(back.to_string(), "1.5000000");
        assert!(serde_json::from_str::<Decimal>("1.5").is_err());
        assert!(serde_json::from_str::<Decimal>("15").is_err());
        assert!(serde_json::from_str::<Decimal>("\"1e5\"").is_err());
    }

    /// Guard: no native floating point anywhere in the Swap Engine.
    #[test]
    fn swap_engine_source_contains_no_float_types() {
        let float_tokens = [concat!("f", "32"), concat!("f", "64")];
        let dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("src/swap");
        let mut stack = vec![dir];
        let mut checked = 0;
        while let Some(path) = stack.pop() {
            for entry in std::fs::read_dir(&path).unwrap() {
                let p = entry.unwrap().path();
                if p.is_dir() {
                    stack.push(p);
                    continue;
                }
                if p.extension().and_then(|e| e.to_str()) != Some("rs") {
                    continue;
                }
                checked += 1;
                let src = std::fs::read_to_string(&p).unwrap();
                for (n, line) in src.lines().enumerate() {
                    if line.trim_start().starts_with("//") {
                        continue; // docs may name the types they forbid
                    }
                    for tok in float_tokens {
                        let hit = line.match_indices(tok).any(|(i, _)| {
                            let before = line[..i].chars().last();
                            let after = line[i + tok.len()..].chars().next();
                            let is_ident = |c: Option<char>| {
                                c.is_some_and(|c| c.is_alphanumeric() || c == '_')
                            };
                            !is_ident(before) && !is_ident(after)
                        });
                        // The serde visitor must name the float type to reject it.
                        let allowed = line.contains("fn visit_");
                        assert!(
                            !hit || allowed,
                            "{}:{} uses a float type; use swap::decimal::Decimal",
                            p.display(),
                            n + 1
                        );
                    }
                }
            }
        }
        assert!(checked > 0);
    }
}
