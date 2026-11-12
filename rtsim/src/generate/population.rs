//! How many NPCs each settlement wants (NH-171): an authored
//! `npc_count` when the settlement's data sets one, otherwise the upstream
//! rule that sizes a town by its plot count.

/// One guard per this many plots (upstream's rule).
const PER_GUARD: u32 = 4;
/// One adventurer per this many plots.
const PER_ADVENTURER: u32 = 5;
/// One merchant per this many plots, on top of the plot count.
const PER_MERCHANT: u32 = 6;

/// How many NPCs of each town role one settlement wants (NH-171).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct SettlementPopulation {
    pub guards: u32,
    pub adventurers: u32,
    pub merchants: u32,
    /// Farmers, herbalists, hunters, blacksmiths, chefs and alchemists.
    pub others: u32,
}

impl SettlementPopulation {
    /// The upstream rule: one guard per 4 plots, one adventurer per 5, the
    /// rest of the plot count as other town NPCs, and merchants on top
    /// (one per 6 plots, plus one).
    pub fn from_plot_count(plots: u32) -> Self {
        let guards = plots / PER_GUARD;
        let adventurers = plots / PER_ADVENTURER;
        Self {
            guards,
            adventurers,
            merchants: plots / PER_MERCHANT + 1,
            others: plots.saturating_sub(guards + adventurers),
        }
    }

    /// An authored total split in the same proportions as
    /// [`Self::from_plot_count`] (merchants are 1/6 on top of the plot count,
    /// i.e. 1/7 of the total, and at least one, as there), adding up to
    /// exactly `total`.
    pub fn from_total(total: u32) -> Self {
        let merchants = if total == 0 {
            0
        } else {
            (total / (PER_MERCHANT + 1)).max(1)
        };
        let rest = total - merchants;
        let guards = rest / PER_GUARD;
        let adventurers = rest / PER_ADVENTURER;
        Self {
            guards,
            adventurers,
            merchants,
            others: rest - guards - adventurers,
        }
    }

    pub fn total(&self) -> u32 { self.guards + self.adventurers + self.merchants + self.others }
}

/// The NPC population a world site wants, if it is a settlement: its
/// authored `npc_count` when the data sets one, otherwise the upstream
/// plot-count rule. `None` for every non-settlement site.
pub fn settlement_population(site: &world::site::Site) -> Option<SettlementPopulation> {
    // TODO: Stupid. Only find site towns
    if !site
        .meta()
        .is_some_and(|m| matches!(m, common::terrain::SiteKindMeta::Settlement(_)))
    {
        return None;
    }
    Some(match site.npc_count {
        Some(total) => SettlementPopulation::from_total(total),
        None => SettlementPopulation::from_plot_count(site.plots().len() as u32),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn settlement_population_splits_keep_today_and_hit_authored_totals() {
        // Today's rule, unchanged.
        let p = SettlementPopulation::from_plot_count(347);
        assert_eq!(
            (p.guards, p.adventurers, p.merchants, p.others),
            (86, 69, 58, 192)
        );
        assert_eq!(SettlementPopulation::from_plot_count(0).total(), 1);
        // Every settlement that wants NPCs gets a merchant, as upstream.
        assert_eq!(SettlementPopulation::from_total(3).merchants, 1);
        assert_eq!(SettlementPopulation::from_total(0).merchants, 0);
        // An authored total is exact, in roughly the same proportions.
        for total in [0, 1, 7, 10, 50, 350, 400, 5_000] {
            assert_eq!(SettlementPopulation::from_total(total).total(), total);
        }
        let k = SettlementPopulation::from_total(350);
        assert_eq!(
            (k.guards, k.adventurers, k.merchants, k.others),
            (75, 60, 50, 165)
        );
    }
}
