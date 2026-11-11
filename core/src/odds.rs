//! Odds math. All functions are pure.
//!
//! Everything that raises (1−p) to the n-th power is done in log space (`ln_1p` / `exp_m1`):
//! the weekly treasures have p around 1e-6, where `1 − (1−p)^n` in plain floating point loses
//! most of its significant digits.

use statrs::distribution::{Binomial, Discrete, DiscreteCDF};

/// Expected number of hits: n·p
pub fn expected(n: u64, p: f64) -> f64 {
    n as f64 * clean_p(p)
}

fn clean_p(p: f64) -> f64 {
    if p.is_finite() {
        p.clamp(0.0, 1.0)
    } else {
        0.0
    }
}

/// ln((1−p)^n) = n·ln(1−p)
fn ln_dry(n: u64, p: f64) -> f64 {
    let p = clean_p(p);
    if n == 0 {
        return 0.0;
    }
    if p >= 1.0 {
        return f64::NEG_INFINITY;
    }
    n as f64 * (-p).ln_1p()
}

/// P(at least one hit in n chests) = 1 − (1−p)^n, evaluated as −expm1(n·ln(1−p)).
pub fn p_at_least_one(n: u64, p: f64) -> f64 {
    (-ln_dry(n, p).exp_m1()).clamp(0.0, 1.0)
}

/// P(zero hits in n chests) = (1−p)^n.
/// Read as "only X% of parties would still have none".
pub fn dry_streak(n: u64, p: f64) -> f64 {
    ln_dry(n, p).exp().clamp(0.0, 1.0)
}

/// Luck percentile = P(K ≤ got) for K ~ Binomial(n, p). Near 0 = unlucky, near 1 = lucky.
pub fn luck_percentile(n: u64, p: f64, got: u64) -> f64 {
    if n == 0 {
        return 0.5;
    }
    let p = clean_p(p);
    if p == 0.0 || got >= n {
        return 1.0;
    }
    if p == 1.0 {
        return 0.0; // got < n, yet every chest is a hit
    }
    if got == 0 {
        return dry_streak(n, p);
    }
    match Binomial::new(p, n) {
        Ok(b) => b.cdf(got).clamp(0.0, 1.0),
        Err(_) => 0.5,
    }
}

/// P(K = got) for K ~ Binomial(n, p), from the log-pmf (no overflow or cancellation for large n).
pub fn pmf(n: u64, p: f64, got: u64) -> f64 {
    let p = clean_p(p);
    if got > n {
        return 0.0;
    }
    if p == 0.0 {
        return if got == 0 { 1.0 } else { 0.0 };
    }
    if p == 1.0 {
        return if got == n { 1.0 } else { 0.0 };
    }
    match Binomial::new(p, n) {
        Ok(b) => b.ln_pmf(got).exp().clamp(0.0, 1.0),
        Err(_) => 0.0,
    }
}

/// Mid-p variant used for the overall luck meter: P(K<got) + ½P(K=got). Symmetric around 0.5,
/// so discrete distributions with large point masses don't look permanently "lucky".
/// Computed as CDF − ½·pmf rather than as the mean of two CDFs: when both CDFs are close to 1
/// their difference (the point mass) would otherwise lose every significant digit.
pub fn luck_midp(n: u64, p: f64, got: u64) -> f64 {
    if n == 0 {
        return 0.5;
    }
    (luck_percentile(n, p, got) - 0.5 * pmf(n, p, got)).clamp(0.0, 1.0)
}

#[derive(Clone, Debug, PartialEq)]
pub struct Stat {
    pub n: u64,
    pub p: f64,
    pub got: u64,
    pub expected: f64,
    pub p_at_least_one: f64,
    pub luck: f64,
    pub dry: f64,
}

pub fn stat(n: u64, p: f64, got: u64) -> Stat {
    Stat {
        n,
        p,
        got,
        expected: expected(n, p),
        p_at_least_one: p_at_least_one(n, p),
        luck: luck_percentile(n, p, got),
        dry: dry_streak(n, p),
    }
}

/// Number formatting for tooltips: enough significant digits that tiny probabilities never
/// print as 0 and values a hair below 100% never print as 100%.
pub mod fmt {
    /// About 4 significant digits; scientific notation below 0.001.
    pub fn num(v: f64) -> String {
        if !v.is_finite() {
            return "–".into();
        }
        if v == 0.0 {
            return "0".into();
        }
        let a = v.abs();
        if (0.001..1e6).contains(&a) {
            let decimals = (3 - a.log10().floor() as i32).clamp(0, 6) as usize;
            let s = format!("{v:.decimals$}");
            if s.contains('.') {
                s.trim_end_matches('0').trim_end_matches('.').to_string()
            } else {
                s
            }
        } else {
            format!("{v:.3e}")
        }
    }

    /// A probability as a percentage.
    pub fn pct(v: f64) -> String {
        if !v.is_finite() {
            return "–".into();
        }
        if v <= 0.0 {
            return "0%".into();
        }
        if v >= 1.0 {
            return "100%".into();
        }
        let x = v * 100.0;
        if x < 0.01 {
            return format!("{x:.2e}%");
        }
        if x > 99.99 {
            // enough decimals to show the gap to 100
            let gap = 100.0 - x;
            let decimals = ((-gap.log10()).ceil() as i32 + 1).clamp(2, 12) as usize;
            let s = format!("{x:.decimals$}");
            if s.starts_with("100") {
                return "99.99…%".into();
            }
            return format!("{s}%");
        }
        format!("{x:.2}%")
    }
}

/// Tooltip text with the formula and the actual numbers plugged in.
pub mod formula {
    use super::fmt::{num, pct};

    pub fn expected(n: u64, p: f64, v: f64) -> String {
        format!("expected = n·p = {n} × {} = {}", num(p), num(v))
    }
    pub fn at_least_one(n: u64, p: f64, v: f64) -> String {
        format!("P(≥1) = 1 − (1−p)^n = 1 − (1−{})^{n} = {}", num(p), pct(v))
    }
    pub fn luck(n: u64, p: f64, got: u64, v: f64) -> String {
        format!(
            "luck = P(K ≤ {got}) with K ~ Binomial(n={n}, p={}) = {}  (50% = average, low = unlucky)",
            num(p),
            pct(v)
        )
    }
    pub fn dry(n: u64, p: f64, v: f64) -> String {
        let v = pct(v);
        format!("dry streak = (1−p)^n = (1−{})^{n} = {v}: only {v} of parties would still have none", num(p))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn close(a: f64, b: f64, eps: f64) -> bool {
        (a - b).abs() < eps
    }

    /// Reference binomial CDF summed term by term in log space with exact log-factorials.
    fn ref_cdf(n: u64, p: f64, got: u64) -> f64 {
        let nn = n as usize;
        let mut ln_fact = vec![0.0f64; nn + 1];
        for i in 1..=nn {
            ln_fact[i] = ln_fact[i - 1] + (i as f64).ln();
        }
        (0..=got.min(n) as usize)
            .map(|k| {
                (ln_fact[nn] - ln_fact[k] - ln_fact[nn - k] + k as f64 * p.ln() + (nn - k) as f64 * (-p).ln_1p()).exp()
            })
            .sum()
    }

    #[test]
    fn at_least_one_spec_example() {
        // p = 0.02, n = 80 -> 0.8013
        assert!(close(p_at_least_one(80, 0.02), 0.8013, 1e-4));
    }

    #[test]
    fn expected_is_np() {
        assert!(close(expected(80, 0.02), 1.6, 1e-12));
    }

    #[test]
    fn dry_streak_complements() {
        assert!(close(dry_streak(80, 0.02) + p_at_least_one(80, 0.02), 1.0, 1e-12));
        assert!(close(dry_streak(0, 0.5), 1.0, 1e-12));
    }

    #[test]
    fn luck_percentile_matches_known_binomial() {
        // Binomial(10, 0.5): P(K<=5) = 638/1024
        assert!(close(luck_percentile(10, 0.5, 5), 638.0 / 1024.0, 1e-9));
        // P(K<=0) = (1-p)^n
        assert!(close(luck_percentile(80, 0.02, 0), dry_streak(80, 0.02), 1e-12));
        assert!(close(luck_percentile(10, 0.5, 10), 1.0, 1e-12));
    }

    #[test]
    fn luck_edge_cases() {
        assert_eq!(luck_percentile(0, 0.3, 0), 0.5);
        assert_eq!(luck_percentile(10, 0.0, 0), 1.0);
        assert_eq!(luck_percentile(10, 1.0, 9), 0.0);
        assert_eq!(luck_percentile(10, 1.0, 10), 1.0);
    }

    #[test]
    fn midp_is_centered() {
        assert!(close(luck_midp(10, 0.5, 5), 0.5, 1e-9));
        assert!(luck_midp(100, 0.1, 0) < 0.01);
        assert!(luck_midp(100, 0.1, 30) > 0.99);
    }

    #[test]
    fn tiny_p_keeps_precision() {
        // 1 in 1,562,500 over 3 chests
        let p = 6.4e-7;
        let exact = 3.0 * p - 3.0 * p * p + p * p * p;
        assert!((p_at_least_one(3, p) - exact).abs() / exact < 1e-12, "{}", p_at_least_one(3, p));
        // plain 1-(1-p)^n returns exactly 0 here; log space does not
        assert!(p_at_least_one(1, 1e-18) > 0.0);
        assert!(close(p_at_least_one(1, 1e-18), 1e-18, 1e-30));
    }

    #[test]
    fn large_n_matches_log_space_reference() {
        for &(n, p, got) in &[
            (100_000u64, 1e-5, 0u64),
            (100_000, 1e-5, 1),
            (100_000, 1e-5, 3),
            (50_000, 0.0017, 90),
            (20_000, 0.3, 6_000),
        ] {
            let a = luck_percentile(n, p, got);
            let b = ref_cdf(n, p, got);
            assert!((a - b).abs() < 1e-9, "n={n} p={p} got={got}: {a} vs {b}");
        }
        // n·p huge: saturates cleanly, never NaN
        assert_eq!(p_at_least_one(u64::MAX / 2, 0.5), 1.0);
        assert_eq!(dry_streak(u64::MAX / 2, 0.5), 0.0);
        assert!(luck_midp(10_000_000, 1e-6, 10).is_finite());
    }

    #[test]
    fn midp_is_accurate_when_cdf_is_near_one() {
        let (n, p, got) = (1000u64, 0.001, 8u64);
        let point = ref_cdf(n, p, got) - ref_cdf(n, p, got - 1);
        assert!(close(pmf(n, p, got), point, 1e-12));
        assert!(close(luck_midp(n, p, got), ref_cdf(n, p, got) - 0.5 * point, 1e-12));
        assert!(close(pmf(10, 0.5, 5), 252.0 / 1024.0, 1e-12));
        assert_eq!(pmf(10, 0.0, 0), 1.0);
        assert_eq!(pmf(10, 1.0, 10), 1.0);
        assert_eq!(pmf(10, 0.5, 11), 0.0);
    }

    #[test]
    fn nan_probability_is_treated_as_zero() {
        assert_eq!(p_at_least_one(10, f64::NAN), 0.0);
        assert_eq!(dry_streak(10, f64::NAN), 1.0);
        assert_eq!(luck_percentile(10, f64::NAN, 0), 1.0);
    }

    #[test]
    fn formula_text_contains_numbers() {
        let s = formula::at_least_one(80, 0.02, p_at_least_one(80, 0.02));
        assert!(s.contains("80") && s.contains("0.02") && s.contains("80.14%"), "{s}");
    }

    #[test]
    fn tooltip_numbers_are_never_rounded_away() {
        use super::fmt::{num, pct};
        assert_eq!(num(6.4e-7), "6.400e-7");
        assert_eq!(num(0.02), "0.02");
        assert_eq!(num(1.6), "1.6");
        assert_eq!(num(0.0), "0");
        assert_eq!(pct(0.5), "50.00%");
        assert_eq!(pct(0.0), "0%");
        assert_eq!(pct(1.92e-6), "1.92e-4%");
        let d = dry_streak(80, 6.4e-7); // 0.9999488...
        let s = pct(d);
        assert!(s.starts_with("99.99") && s != "100%", "{s}");
        // the old tooltips printed p = 0.0000 and expected = 0.00 for the weekly Gold Outfit
        let s = formula::expected(80, 6.4e-7, expected(80, 6.4e-7));
        assert!(s.contains("6.400e-7") && s.contains("5.120e-5"), "{s}");
        assert!(!formula::dry(80, 6.4e-7, d).contains("100"), "{}", formula::dry(80, 6.4e-7, d));
    }
}
