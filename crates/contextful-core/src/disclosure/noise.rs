//! `disclosure.release.noise-mechanism`: exact two-sided geometric noise.
//!
//! The sampler draws the discrete Laplace distribution, `P(x) ∝ exp(-|x| / scale)` over
//! the integers, from uniform integers alone: every probability is a ratio of integers,
//! so no floating-point tail biases the spend a release claims. The construction is the
//! rejection sampler of Canonne, Kamath and Steinke for a rational scale.
//!
//! A statistic with sensitivity `Δ` under a per-statistic epsilon `ε` takes scale `Δ / ε`.
//! A release dividing its per-run spend among `k` statistics gives each `ε / k`.

use std::num::NonZeroU64;

/// Micro-epsilon units in one epsilon.
pub const MICROS_PER_EPSILON: u64 = 1_000_000;

/// A privacy spend in micro-epsilon units: integer, so a ledger sums spends exactly, and
/// above zero, so every noise scale is finite.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct Spend(NonZeroU64);

impl Spend {
    /// `micros` micro-epsilon; `None` for zero.
    pub fn from_micros(micros: u64) -> Option<Spend> {
        NonZeroU64::new(micros).map(Spend)
    }

    /// A whole number of epsilon; `None` for zero or past `u64` micro-epsilon.
    pub fn epsilon(whole: u64) -> Option<Spend> {
        whole.checked_mul(MICROS_PER_EPSILON).and_then(Spend::from_micros)
    }

    pub fn micros(self) -> u64 {
        self.0.get()
    }
}

/// A source of uniform 64-bit words. A release draws all its noise from one.
pub trait Entropy {
    fn next_u64(&mut self) -> u64;
}

/// A nonnegative rational `num / den`, `den > 0`.
#[derive(Debug, Clone, Copy)]
struct Ratio {
    num: u128,
    den: u128,
}

fn gcd(mut a: u128, mut b: u128) -> u128 {
    while b != 0 {
        (a, b) = (b, a % b);
    }
    a
}

impl Ratio {
    fn new(num: u128, den: u128) -> Ratio {
        let g = gcd(num, den).max(1);
        Ratio { num: num / g, den: den / g }
    }
}

/// A uniform integer in `[0, n)`, `n > 0`, by rejection over 128-bit words.
fn uniform_below(rng: &mut dyn Entropy, n: u128) -> u128 {
    let zone = u128::MAX - (u128::MAX % n);
    loop {
        let word = ((rng.next_u64() as u128) << 64) | rng.next_u64() as u128;
        if word < zone {
            return word % n;
        }
    }
}

/// `Bernoulli(p)` for a rational `p` in `[0, 1]`.
fn bernoulli(rng: &mut dyn Entropy, p: Ratio) -> bool {
    uniform_below(rng, p.den) < p.num
}

/// `Bernoulli(exp(-x))` for a rational `x` in `[0, 1]`.
fn bernoulli_exp_unit(rng: &mut dyn Entropy, x: Ratio) -> bool {
    let mut k: u128 = 1;
    // Each step succeeds with probability x / k; a denominator past u128 ends the run.
    while let Some(den) = x.den.checked_mul(k) {
        if !bernoulli(rng, Ratio::new(x.num, den)) {
            break;
        }
        k += 1;
    }
    k % 2 == 1
}

/// `Bernoulli(exp(-x))` for a rational `x ≥ 0`.
fn bernoulli_exp(rng: &mut dyn Entropy, mut x: Ratio) -> bool {
    while x.num > x.den {
        if !bernoulli_exp_unit(rng, Ratio { num: 1, den: 1 }) {
            return false;
        }
        x.num -= x.den;
    }
    bernoulli_exp_unit(rng, x)
}

/// `Geometric(1 - exp(-1))`: the count of successes before the first failure.
fn geometric_exp_one(rng: &mut dyn Entropy) -> u128 {
    let mut k = 0;
    while bernoulli_exp(rng, Ratio { num: 1, den: 1 }) {
        k += 1;
    }
    k
}

/// `Geometric(1 - exp(-x))` for a rational `x = s / t > 0`.
fn geometric_exp(rng: &mut dyn Entropy, x: Ratio) -> u128 {
    let (s, t) = (x.num, x.den);
    let u = loop {
        let u = uniform_below(rng, t);
        if bernoulli_exp(rng, Ratio::new(u, t)) {
            break u;
        }
    };
    let v = geometric_exp_one(rng);
    v.saturating_mul(t).saturating_add(u) / s
}

/// One draw of the discrete Laplace distribution with scale `scale_num / scale_den`:
/// `P(x) ∝ exp(-|x| · scale_den / scale_num)`. Both parts are above zero.
pub fn discrete_laplace(rng: &mut dyn Entropy, scale_num: u128, scale_den: u128) -> i64 {
    assert!(scale_num > 0 && scale_den > 0, "a discrete Laplace scale is a positive ratio");
    let inverse = Ratio::new(scale_den, scale_num);
    loop {
        let negative = bernoulli(rng, Ratio { num: 1, den: 2 });
        let magnitude = geometric_exp(rng, inverse);
        if negative && magnitude == 0 {
            continue;
        }
        let magnitude = i64::try_from(magnitude).unwrap_or(i64::MAX);
        return if negative { -magnitude } else { magnitude };
    }
}

/// The noise one statistic of sensitivity `sensitivity` takes when a release divides
/// `spend` evenly among `statistics` statistics: scale `sensitivity · statistics / ε`.
pub fn statistic_noise(rng: &mut dyn Entropy, sensitivity: u64, spend: Spend, statistics: u64) -> i64 {
    let num = sensitivity.max(1) as u128 * statistics.max(1) as u128 * MICROS_PER_EPSILON as u128;
    discrete_laplace(rng, num, spend.micros() as u128)
}
