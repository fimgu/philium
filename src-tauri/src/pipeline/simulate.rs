//! Synthetic load for the queue: fake pull requests, some bad, through batching
//! and bisection. It tunes batch size and proves bisection never lands a bad
//! change and never throws out a good one.
use serde::Serialize;

#[derive(Debug, Clone)]
pub struct Sim {
    pub prs: usize,
    pub fail_rate: f64,
    pub batch_max: usize,
    pub seed: u64,
}

#[derive(Debug, Clone, Serialize, PartialEq)]
pub struct SimReport {
    pub prs: usize,
    pub landed: usize,
    pub ejected: usize,
    /// Gate runs used in total.
    pub runs: usize,
    pub wrongly_landed: usize,
    pub wrongly_ejected: usize,
    pub mean_batch: f64,
    pub runs_per_pr: f64,
}

struct Rng(u64);
impl Rng {
    fn next(&mut self) -> f64 {
        // xorshift64*: deterministic for a seed, no dependency.
        self.0 ^= self.0 >> 12;
        self.0 ^= self.0 << 25;
        self.0 ^= self.0 >> 27;
        (self.0.wrapping_mul(0x2545F4914F6CDD1D) >> 11) as f64 / (1u64 << 53) as f64
    }
}

/// Batch size from the recent failure rate: one at a time above 20 %.
pub fn batch_size(recent: &[bool], max: usize) -> usize {
    if recent.is_empty() {
        return max.max(1);
    }
    let failed = recent.iter().filter(|f| **f).count() as f64 / recent.len() as f64;
    if failed > 0.2 { 1 } else { max.max(1) }
}

/// Test a batch; on failure split it and test the halves until the culprits
/// stand alone. Returns (landed, ejected, runs).
pub fn bisect(batch: &[(usize, bool)]) -> (Vec<usize>, Vec<usize>, usize) {
    let bad = batch.iter().any(|(_, b)| *b);
    if !bad {
        return (batch.iter().map(|x| x.0).collect(), vec![], 1);
    }
    if batch.len() == 1 {
        return (vec![], vec![batch[0].0], 1);
    }
    let (l, r) = batch.split_at(batch.len() / 2);
    let (mut a, mut e, ra) = bisect(l);
    let (b, f, rb) = bisect(r);
    a.extend(b);
    e.extend(f);
    (a, e, 1 + ra + rb)
}

pub fn simulate(s: &Sim) -> SimReport {
    let mut rng = Rng(s.seed.max(1));
    let prs: Vec<(usize, bool)> = (0..s.prs).map(|i| (i, rng.next() < s.fail_rate)).collect();
    let mut queue = prs.as_slice();
    let mut recent: Vec<bool> = vec![];
    let (mut landed, mut ejected, mut runs, mut batches) = (vec![], vec![], 0, 0);
    while !queue.is_empty() {
        let n = batch_size(&recent[recent.len().saturating_sub(10)..], s.batch_max).min(queue.len());
        let (batch, rest) = queue.split_at(n);
        let (l, e, r) = bisect(batch);
        recent.push(!e.is_empty());
        landed.extend(l);
        ejected.extend(e);
        runs += r;
        batches += 1;
        queue = rest;
    }
    let bad = |i: &usize| prs[*i].1;
    SimReport {
        prs: s.prs,
        landed: landed.len(),
        ejected: ejected.len(),
        runs,
        wrongly_landed: landed.iter().filter(|i| bad(i)).count(),
        wrongly_ejected: ejected.iter().filter(|i| !bad(i)).count(),
        mean_batch: if batches == 0 { 0.0 } else { s.prs as f64 / batches as f64 },
        runs_per_pr: if s.prs == 0 { 0.0 } else { runs as f64 / s.prs as f64 },
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bisection_ejects_exactly_the_bad_changes_at_any_failure_rate() {
        for seed in 1..40 {
            for rate in [0.0, 0.05, 0.2, 0.5, 1.0] {
                let r = simulate(&Sim { prs: 57, fail_rate: rate, batch_max: 8, seed });
                assert_eq!((r.wrongly_landed, r.wrongly_ejected), (0, 0), "seed {seed} rate {rate}");
                assert_eq!(r.landed + r.ejected, 57);
            }
        }
        let (l, e, runs) = bisect(&[(0, false), (1, true), (2, false), (3, false)]);
        assert_eq!((l, e), (vec![0, 2, 3], vec![1]));
        assert!(runs <= 5);
    }

    #[test]
    fn batches_shrink_to_one_when_failures_are_common_and_save_runs_when_rare() {
        assert_eq!(batch_size(&[true, false, false, false], 8), 1);
        assert_eq!(batch_size(&[true, false, false, false, false, false], 8), 8);
        assert_eq!(batch_size(&[], 4), 4);
        let calm = simulate(&Sim { prs: 200, fail_rate: 0.02, batch_max: 8, seed: 7 });
        let one = simulate(&Sim { prs: 200, fail_rate: 0.02, batch_max: 1, seed: 7 });
        assert!(calm.runs < one.runs, "batching saves gate runs when changes are mostly good: {} vs {}", calm.runs, one.runs);
        let stormy = simulate(&Sim { prs: 200, fail_rate: 0.5, batch_max: 8, seed: 7 });
        assert!(stormy.mean_batch < 1.5, "{}", stormy.mean_batch);
    }
}
