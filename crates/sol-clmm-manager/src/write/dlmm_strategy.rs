//! Meteora DLMM bin-distribution strategies (5 product modes).

/// How to spread liquidity across bins when depositing into a DLMM position.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum DlmmStrategy {
    /// Curve — weight toward the middle of the range (stable / low vol).
    #[default]
    Curve,
    /// Bid-Ask — weight toward the edges (vol / DCA-style).
    BidAsk,
    /// Spot concentrated — flat across a tight 1–3 bin window around active.
    SpotConcentrated,
    /// Spot spread — flat across a moderate ~20–30 bin window.
    SpotSpread,
    /// Spot wide — flat across a wide ~50 bin window.
    SpotWide,
}

impl DlmmStrategy {
    pub const ALL: [DlmmStrategy; 5] = [
        Self::Curve,
        Self::BidAsk,
        Self::SpotConcentrated,
        Self::SpotSpread,
        Self::SpotWide,
    ];

    pub fn label(self) -> &'static str {
        match self {
            Self::Curve => "curve · peak at mid-range",
            Self::BidAsk => "bid-ask · weight at edges",
            Self::SpotConcentrated => "spot concentrated · 1–3 bins",
            Self::SpotSpread => "spot spread · ~20–30 bins",
            Self::SpotWide => "spot wide · ~50 bins",
        }
    }

    pub fn short(self) -> &'static str {
        match self {
            Self::Curve => "curve",
            Self::BidAsk => "bid-ask",
            Self::SpotConcentrated => "spot-tight",
            Self::SpotSpread => "spot-spread",
            Self::SpotWide => "spot-wide",
        }
    }

    /// Suggested half-width in bins around `active_id` when the UI does not pick a custom range.
    pub fn default_half_width(self) -> i32 {
        match self {
            Self::Curve => 12,
            Self::BidAsk => 20,
            Self::SpotConcentrated => 1,
            Self::SpotSpread => 12,
            Self::SpotWide => 25,
        }
    }

    pub fn next(self) -> Self {
        match self {
            Self::Curve => Self::BidAsk,
            Self::BidAsk => Self::SpotConcentrated,
            Self::SpotConcentrated => Self::SpotSpread,
            Self::SpotSpread => Self::SpotWide,
            Self::SpotWide => Self::Curve,
        }
    }
}

/// Per-bin relative weights (sum may be any positive; caller normalizes).
#[derive(Debug, Clone)]
pub struct BinWeights {
    pub bins: Vec<(i32, f64)>,
}

/// Build weights for `[min_bin, max_bin]` inclusive around strategy shape.
pub fn distribute_bins(strategy: DlmmStrategy, min_bin: i32, max_bin: i32) -> BinWeights {
    if min_bin > max_bin {
        return BinWeights { bins: Vec::new() };
    }
    let mid = (min_bin as i64 + max_bin as i64) / 2;
    let half = ((max_bin - min_bin) as f64 / 2.0).max(1.0);
    let mut bins = Vec::with_capacity((max_bin - min_bin + 1) as usize);
    for id in min_bin..=max_bin {
        let dist = (id as i64 - mid).unsigned_abs() as f64;
        let w = match strategy {
            DlmmStrategy::Curve => {
                let t = (1.0 - dist / half).max(0.0);
                t * t
            }
            DlmmStrategy::BidAsk => {
                // Inverse curve: more weight at edges.
                let t = (dist / half).min(1.0);
                0.05 + t * t
            }
            DlmmStrategy::SpotConcentrated | DlmmStrategy::SpotSpread | DlmmStrategy::SpotWide => {
                1.0
            }
        };
        if w > 0.0 {
            bins.push((id, w));
        }
    }
    BinWeights { bins }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn curve_peaks_at_mid() {
        let w = distribute_bins(DlmmStrategy::Curve, -5, 5);
        let mid = w.bins.iter().find(|(id, _)| *id == 0).unwrap().1;
        let edge = w.bins.iter().find(|(id, _)| *id == 5).unwrap().1;
        assert!(mid > edge);
    }

    #[test]
    fn bid_ask_peaks_at_edge() {
        let w = distribute_bins(DlmmStrategy::BidAsk, -5, 5);
        let mid = w.bins.iter().find(|(id, _)| *id == 0).unwrap().1;
        let edge = w.bins.iter().find(|(id, _)| *id == 5).unwrap().1;
        assert!(edge > mid);
    }

    #[test]
    fn spot_flat() {
        let w = distribute_bins(DlmmStrategy::SpotSpread, 0, 4);
        let first = w.bins[0].1;
        assert!(w.bins.iter().all(|(_, x)| (*x - first).abs() < 1e-9));
    }
}
