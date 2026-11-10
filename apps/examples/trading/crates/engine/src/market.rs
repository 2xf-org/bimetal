//! Simulated prices: a seeded random walk of each listing's bid.

use trading_core::Quote;

use crate::{ConfigError, Listing};

/// The most cents that a bid moves between two quotes.
const MAX_STEP: u64 = 2;

/// The listings' latest quotes, and the generator that moves them.
pub(crate) struct Market {
    random: SplitMix64,
    quotes: Vec<Quote>,
}

impl Market {
    /// Open the market at the listings' prices, as quote 0.
    pub(crate) fn open(listings: &[Listing], seed: u64) -> Result<Self, ConfigError> {
        let mut quotes: Vec<Quote> = Vec::with_capacity(listings.len());
        for listing in listings {
            if quotes.iter().any(|quote| quote.symbol == listing.symbol) {
                return Err(ConfigError::DuplicateSymbol {
                    symbol: listing.symbol.clone(),
                });
            }
            quotes.push(Quote {
                sequence: 0,
                symbol: listing.symbol.clone(),
                bid: listing.price,
                ask: listing.price.saturating_add(1),
            });
        }
        Ok(Self {
            random: SplitMix64(seed),
            quotes,
        })
    }

    /// The latest quote of each listing.
    pub(crate) fn quotes(&self) -> &[Quote] {
        &self.quotes
    }

    /// Move every bid by up to `MAX_STEP` cents, keeping it at 1 cent or more,
    /// and number the new quotes one higher.
    pub(crate) fn step(&mut self) {
        for quote in &mut self.quotes {
            let draw = self.random.next_u64() % (2 * MAX_STEP + 1);
            let bid = quote.bid.saturating_add(draw).saturating_sub(MAX_STEP);
            quote.sequence += 1;
            quote.bid = bid.max(1);
            quote.ask = quote.bid.saturating_add(1);
        }
    }
}

/// SplitMix64, a small generator whose output depends only on its seed.
struct SplitMix64(u64);

impl SplitMix64 {
    fn next_u64(&mut self) -> u64 {
        self.0 = self.0.wrapping_add(0x9e37_79b9_7f4a_7c15);
        let mut mixed = self.0;
        mixed = (mixed ^ (mixed >> 30)).wrapping_mul(0xbf58_476d_1ce4_e5b9);
        mixed = (mixed ^ (mixed >> 27)).wrapping_mul(0x94d0_49bb_1331_11eb);
        mixed ^ (mixed >> 31)
    }
}
