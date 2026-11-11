//! Odds math. All functions are pure.

use statrs::distribution::{Binomial, DiscreteCDF};

/// Expected number of hits: n·p
pub fn expected(n: u64, p: f64) -> f64 {
    n as f64 * p
}

/// P(at least one hit in n chests) = 1 − (1−p)^n
pub fn p_at_least_one(n: u64, p: f64) -> f64 {
    1.0 - dry_streak(n, p)
}

/// P(zero hits in n chests) = (1−p)^n.
/// Read as "only X% of parties would still have none".
pub fn dry_streak(n: u64, p: f64) -> f64 {
    (1.0 - p.clamp(0.0, 1.0)).powf(n as f64)
}

/// Luck percentile = P(K ≤ got) for K ~ Binomial(n, p). Near 0 = unlucky, near 1 = lucky.
pub fn luck_percentile(n: u64, p: f64, got: u64) -> f64 {
    if n == 0 {
        return 0.5;
    }
    let p = p.clamp(0.0, 1.0);
    if p == 0.0 {
        return 1.0;
    }
    if p == 1.0 {
        return if got >= n { 1.0 } else { 0.0 };
    }
    if got >= n {
        return 1.0;
    }
    match Binomial::new(p, n) {
        Ok(b) => b.cdf(got).clamp(0.0, 1.0),
        Err(_) => 0.5,
    }
}

/// Mid-p variant used for the overall luck meter: P(K<got) + ½P(K=got). Symmetric around 0.5,
/// so discrete distributions with large point masses don't look permanently "lucky".
pub fn luck_midp(n: u64, p: f64, got: u64) -> f64 {
    if n == 0 {
        return 0.5;
    }
    let cdf = luck_percentile(n, p, got);
    let below = if got == 0 { 0.0 } else { luck_percentile(n, p, got - 1) };
    ((cdf + below) / 2.0).clamp(0.0, 1.0)
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

/// Tooltip text with the formula and the actual numbers plugged in.
pub mod formula {
    pub fn expected(n: u64, p: f64, v: f64) -> String {
        format!("expected = n·p = {n} × {p:.4} = {v:.2}")
    }
    pub fn at_least_one(n: u64, p: f64, v: f64) -> String {
        format!("P(≥1) = 1 − (1−p)^n = 1 − (1−{p:.4})^{n} = {:.2}%", v * 100.0)
    }
    pub fn luck(n: u64, p: f64, got: u64, v: f64) -> String {
        format!(
            "luck = P(K ≤ {got}) with K ~ Binomial(n={n}, p={p:.4}) = {:.1}%  (50% = average, low = unlucky)",
            v * 100.0
        )
    }
    pub fn dry(n: u64, p: f64, v: f64) -> String {
        format!(
            "dry streak = (1−p)^n = (1−{p:.4})^{n} = {:.2}%: only {:.1}% of parties would still have none",
            v * 100.0,
            v * 100.0
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn close(a: f64, b: f64, eps: f64) -> bool {
        (a - b).abs() < eps
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
        assert!(close(luck_percentile(80, 0.02, 0), dry_streak(80, 0.02), 1e-9));
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
    fn formula_text_contains_numbers() {
        let s = formula::at_least_one(80, 0.02, p_at_least_one(80, 0.02));
        assert!(s.contains("80") && s.contains("0.0200") && s.contains("80.14%"), "{s}");
    }
}
