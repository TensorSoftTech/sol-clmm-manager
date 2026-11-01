//! Which concentrated-liquidity venue a position / pool belongs to.

/// On-chain CLMM / DLMM program family (TUI list + agent tags).
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Default)]
pub enum Venue {
    #[default]
    OrcaWhirlpool,
    RaydiumClmm,
    MeteoraDlmm,
}

impl Venue {
    pub const ALL: [Venue; 3] = [
        Venue::OrcaWhirlpool,
        Venue::RaydiumClmm,
        Venue::MeteoraDlmm,
    ];

    /// Brand: `Orca` · `Raydium` · `Meteora`.
    pub fn brand(self) -> &'static str {
        match self {
            Self::OrcaWhirlpool => "Orca",
            Self::RaydiumClmm => "Raydium",
            Self::MeteoraDlmm => "Meteora",
        }
    }

    /// Product line: `Whirlpool` · `CLMM` · `DLMM`.
    pub fn product(self) -> &'static str {
        match self {
            Self::OrcaWhirlpool => "Whirlpool",
            Self::RaydiumClmm => "CLMM",
            Self::MeteoraDlmm => "DLMM",
        }
    }

    /// Compact tag for dense tables: `orca` · `raydium` · `meteora`.
    pub fn short(self) -> &'static str {
        match self {
            Self::OrcaWhirlpool => "orca",
            Self::RaydiumClmm => "raydium",
            Self::MeteoraDlmm => "meteora",
        }
    }

    /// Full label: `Orca Whirlpool` · `Raydium CLMM` · `Meteora DLMM`.
    pub fn full(self) -> String {
        format!("{} {}", self.brand(), self.product())
    }
}

/// TUI / agent filter: all venues, or one venue at a time (`v` cycles in the TUI).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum VenueFilter {
    #[default]
    All,
    Only(Venue),
}

impl VenueFilter {
    pub fn label(self) -> String {
        match self {
            Self::All => "all venues".into(),
            Self::Only(v) => v.full(),
        }
    }

    pub fn short(self) -> String {
        match self {
            Self::All => "all".into(),
            Self::Only(v) => v.short().into(),
        }
    }

    pub fn matches(self, venue: Venue) -> bool {
        match self {
            Self::All => true,
            Self::Only(v) => v == venue,
        }
    }

    /// `all` → Orca → Raydium → Meteora → `all`.
    pub fn next(self) -> Self {
        match self {
            Self::All => Self::Only(Venue::OrcaWhirlpool),
            Self::Only(Venue::OrcaWhirlpool) => Self::Only(Venue::RaydiumClmm),
            Self::Only(Venue::RaydiumClmm) => Self::Only(Venue::MeteoraDlmm),
            Self::Only(Venue::MeteoraDlmm) => Self::All,
        }
    }

    pub fn is_implemented(self) -> bool {
        true
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn labels() {
        assert_eq!(Venue::OrcaWhirlpool.full(), "Orca Whirlpool");
        assert_eq!(Venue::RaydiumClmm.full(), "Raydium CLMM");
        assert_eq!(Venue::MeteoraDlmm.full(), "Meteora DLMM");
        assert_eq!(Venue::OrcaWhirlpool.short(), "orca");
    }

    #[test]
    fn filter_cycles() {
        let mut f = VenueFilter::All;
        f = f.next();
        assert_eq!(f, VenueFilter::Only(Venue::OrcaWhirlpool));
        f = f.next();
        assert_eq!(f, VenueFilter::Only(Venue::RaydiumClmm));
        assert!(f.is_implemented());
        f = f.next();
        assert_eq!(f, VenueFilter::Only(Venue::MeteoraDlmm));
        f = f.next();
        assert_eq!(f, VenueFilter::All);
    }
}
