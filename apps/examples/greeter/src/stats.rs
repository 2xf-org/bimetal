//! Numeric helpers. The module becomes the `stats` host namespace.

/// Scale samples so that the largest magnitude becomes 1.
#[bimetal::export]
#[bimetal(returns(buffer))]
#[must_use]
pub fn normalize(#[bimetal(buffer)] samples: &[f64]) -> Vec<f64> {
    let peak = samples
        .iter()
        .fold(0.0_f64, |peak, sample| peak.max(sample.abs()));
    if peak == 0.0 {
        return samples.to_vec();
    }
    samples.iter().map(|sample| sample / peak).collect()
}
