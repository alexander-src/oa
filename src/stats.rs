use serde::{Deserialize, Serialize};

/// Summary statistics for a series of timing samples (in milliseconds).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SampleSummary {
    pub count: usize,
    pub min_ms: f64,
    pub median_ms: f64,
    pub mean_ms: f64,
    pub p95_ms: f64,
    pub max_ms: f64,
    pub mad_ms: f64,
    pub iqr_ms: f64,
    pub cv_pct: f64,
}

/// Statistical verdict comparing a candidate against a reference baseline.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub enum BenchVerdict {
    Faster,
    Noise,
    Slower,
}

/// Paired interleaved A/B statistical comparison result.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ComparisonStats {
    pub baseline: SampleSummary,
    pub candidate: SampleSummary,
    pub speedup_median: f64,
    pub paired_ratio_median: f64,
    pub ci95_low: f64,
    pub ci95_high: f64,
    pub mann_whitney_p: f64,
    pub verdict: BenchVerdict,
    pub reason: String,
}

pub fn percentile_sorted(sorted: &[f64], p: f64) -> f64 {
    if sorted.is_empty() {
        return 0.0;
    }
    if sorted.len() == 1 {
        return sorted[0];
    }
    let idx = p.clamp(0.0, 1.0) * (sorted.len() - 1) as f64;
    let lo = idx.floor() as usize;
    let hi = idx.ceil() as usize;
    if lo == hi {
        sorted[lo]
    } else {
        let frac = idx - lo as f64;
        sorted[lo] * (1.0 - frac) + sorted[hi] * frac
    }
}

pub fn summarize(samples: &[f64]) -> SampleSummary {
    if samples.is_empty() {
        return SampleSummary {
            count: 0,
            min_ms: 0.0,
            median_ms: 0.0,
            mean_ms: 0.0,
            p95_ms: 0.0,
            max_ms: 0.0,
            mad_ms: 0.0,
            iqr_ms: 0.0,
            cv_pct: 0.0,
        };
    }
    let mut sorted = samples.to_vec();
    sorted.sort_by(|a, b| a.total_cmp(b));
    let n = sorted.len() as f64;
    let mean = sorted.iter().sum::<f64>() / n;
    let median = percentile_sorted(&sorted, 0.5);
    let q1 = percentile_sorted(&sorted, 0.25);
    let q3 = percentile_sorted(&sorted, 0.75);
    let p95 = percentile_sorted(&sorted, 0.95);
    let mut abs_devs: Vec<f64> = sorted.iter().map(|&v| (v - median).abs()).collect();
    abs_devs.sort_by(|a, b| a.total_cmp(b));
    let mad = percentile_sorted(&abs_devs, 0.5);

    let variance = if sorted.len() > 1 {
        sorted.iter().map(|&v| (v - mean).powi(2)).sum::<f64>() / (n - 1.0)
    } else {
        0.0
    };
    let stddev = variance.sqrt();
    let cv_pct = if mean > 1e-12 {
        (stddev / mean) * 100.0
    } else {
        0.0
    };

    SampleSummary {
        count: sorted.len(),
        min_ms: sorted[0],
        median_ms: median,
        mean_ms: mean,
        p95_ms: p95,
        max_ms: *sorted.last().unwrap_or(&0.0),
        mad_ms: mad,
        iqr_ms: q3 - q1,
        cv_pct,
    }
}

/// Abramowitz & Stegun 7.1.26 error function approximation (max error < 1.5e-7).
fn erfc_approx(x: f64) -> f64 {
    let z = x.abs();
    let t = 1.0 / (1.0 + 0.5 * z);
    let ans = t
        * (-z * z - 1.26551223
            + t * (1.00002368
                + t * (0.37409196
                    + t * (0.09678418
                        + t * (-0.18628806
                            + t * (0.27886807
                                + t * (-1.13520398
                                    + t * (1.48851587 + t * (-0.82215223 + t * 0.17087277)))))))))
            .exp();
    if x >= 0.0 {
        ans
    } else {
        2.0 - ans
    }
}

/// Two-sided Mann-Whitney U p-value with tie correction and continuity correction.
pub fn mann_whitney_u_pvalue(a: &[f64], b: &[f64]) -> f64 {
    let n1 = a.len();
    let n2 = b.len();
    if n1 == 0 || n2 == 0 {
        return 1.0;
    }

    let mut combined: Vec<(f64, u8)> = Vec::with_capacity(n1 + n2);
    for &v in a {
        combined.push((v, 0));
    }
    for &v in b {
        combined.push((v, 1));
    }
    combined.sort_by(|x, y| x.0.total_cmp(&y.0));

    let total = combined.len();
    let mut rank_sum_a = 0.0;
    let mut tie_term = 0.0;
    let mut i = 0;
    while i < total {
        let mut j = i + 1;
        while j < total && (combined[j].0 - combined[i].0).abs() <= 1e-12 {
            j += 1;
        }
        let tie_count = (j - i) as f64;
        if tie_count > 1.0 {
            tie_term += tie_count.powi(3) - tie_count;
        }
        let avg_rank = (i + 1 + j) as f64 / 2.0;
        for item in &combined[i..j] {
            if item.1 == 0 {
                rank_sum_a += avg_rank;
            }
        }
        i = j;
    }

    let n1f = n1 as f64;
    let n2f = n2 as f64;
    let nf = (n1 + n2) as f64;
    let u1 = rank_sum_a - n1f * (n1f + 1.0) / 2.0;
    let u2 = n1f * n2f - u1;
    let u = u1.min(u2);
    let mu = n1f * n2f / 2.0;

    let var_factor = if nf > 1.0 {
        (nf + 1.0) - tie_term / (nf * (nf - 1.0))
    } else {
        0.0
    };
    let sigma = ((n1f * n2f / 12.0) * var_factor.max(0.0)).sqrt();
    if sigma <= 1e-12 {
        return 1.0;
    }
    let z = ((mu - u - 0.5).max(0.0)) / sigma;
    erfc_approx(z / std::f64::consts::SQRT_2).clamp(0.0, 1.0)
}

struct Xorshift64(u64);

impl Xorshift64 {
    fn new(seed: u64) -> Self {
        Self(if seed == 0 { 0x9e3779b97f4a7c15 } else { seed })
    }

    fn next_usize(&mut self, bound: usize) -> usize {
        let mut x = self.0;
        x ^= x << 13;
        x ^= x >> 7;
        x ^= x << 17;
        self.0 = x;
        (x as usize) % bound
    }
}

/// Deterministic 2,000-resample bootstrap 95% confidence interval for the speedup ratio (`baseline / candidate`).
pub fn bootstrap_speedup_ci(baseline: &[f64], candidate: &[f64], resamples: usize) -> (f64, f64) {
    if baseline.is_empty() || candidate.is_empty() {
        return (1.0, 1.0);
    }
    let paired = baseline.len() == candidate.len();
    let n = baseline.len();
    let mut rng = Xorshift64::new(0x0a5e_ed20_2611_2601);
    let mut ratios = Vec::with_capacity(resamples);
    let mut sa = Vec::with_capacity(n);
    let mut sb = Vec::with_capacity(candidate.len());

    for _ in 0..resamples {
        sa.clear();
        sb.clear();
        if paired {
            let mut pair_ratios = Vec::with_capacity(n);
            for _ in 0..n {
                let idx = rng.next_usize(n);
                let a = baseline[idx];
                let b = candidate[idx].max(1e-9);
                pair_ratios.push(a / b);
            }
            pair_ratios.sort_by(|x, y| x.total_cmp(y));
            ratios.push(percentile_sorted(&pair_ratios, 0.5));
        } else {
            for _ in 0..baseline.len() {
                sa.push(baseline[rng.next_usize(baseline.len())]);
            }
            for _ in 0..candidate.len() {
                sb.push(candidate[rng.next_usize(candidate.len())]);
            }
            sa.sort_by(|x, y| x.total_cmp(y));
            sb.sort_by(|x, y| x.total_cmp(y));
            let ma = percentile_sorted(&sa, 0.5);
            let mb = percentile_sorted(&sb, 0.5).max(1e-9);
            ratios.push(ma / mb);
        }
    }

    ratios.sort_by(|x, y| x.total_cmp(y));
    (
        percentile_sorted(&ratios, 0.025),
        percentile_sorted(&ratios, 0.975),
    )
}

/// Compares interleaved baseline vs candidate timing samples (ms) using strict significance thresholds.
pub fn compare_samples(
    baseline_ms: &[f64],
    candidate_ms: &[f64],
    min_speedup: f64,
    alpha: f64,
) -> ComparisonStats {
    let baseline = summarize(baseline_ms);
    let candidate = summarize(candidate_ms);
    let speedup_median = if candidate.median_ms > 1e-9 {
        baseline.median_ms / candidate.median_ms
    } else {
        1.0
    };

    let paired_ratio_median = if baseline_ms.len() == candidate_ms.len() && !baseline_ms.is_empty()
    {
        let mut pair_ratios: Vec<f64> = baseline_ms
            .iter()
            .zip(candidate_ms.iter())
            .map(|(&a, &b)| a / b.max(1e-9))
            .collect();
        pair_ratios.sort_by(|x, y| x.total_cmp(y));
        percentile_sorted(&pair_ratios, 0.5)
    } else {
        speedup_median
    };

    let (ci95_low, ci95_high) = bootstrap_speedup_ci(baseline_ms, candidate_ms, 2000);
    let mann_whitney_p = mann_whitney_u_pvalue(baseline_ms, candidate_ms);

    let (verdict, reason) = if ci95_low >= min_speedup && mann_whitney_p <= alpha {
        (
            BenchVerdict::Faster,
            format!(
                "verified {:.2}x speedup (95% CI [{:.2}x, {:.2}x], p={:.4})",
                paired_ratio_median, ci95_low, ci95_high, mann_whitney_p
            ),
        )
    } else if ci95_high <= (1.0 / min_speedup) && mann_whitney_p <= alpha {
        (
            BenchVerdict::Slower,
            format!(
                "regression {:.2}x (95% CI [{:.2}x, {:.2}x], p={:.4})",
                paired_ratio_median, ci95_low, ci95_high, mann_whitney_p
            ),
        )
    } else {
        (
            BenchVerdict::Noise,
            format!(
                "not statistically significant ({:.2}x, 95% CI [{:.2}x, {:.2}x], p={:.4}, threshold >={:.2}x at alpha={:.3})",
                paired_ratio_median, ci95_low, ci95_high, mann_whitney_p, min_speedup, alpha
            ),
        )
    };

    ComparisonStats {
        baseline,
        candidate,
        speedup_median,
        paired_ratio_median,
        ci95_low,
        ci95_high,
        mann_whitney_p,
        verdict,
        reason,
    }
}
