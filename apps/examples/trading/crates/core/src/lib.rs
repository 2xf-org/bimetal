//! The models of the trading API: orders and their outcomes, quotes, the
//! account, and the errors that hosts handle by code. Amounts and prices are
//! whole cents in a `u64`. Hosts see this package as the `core` namespace.

use std::collections::BTreeMap;

use bimetal::Model;
use serde::{Deserialize, Serialize};

/// Which way an order trades.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Model)]
#[serde(rename_all = "snake_case")]
pub enum Side {
    /// Pay cash for units, at the ask.
    Buy,
    /// Sell units that the account holds, at the bid.
    Sell,
}

/// An immediate-or-cancel order: it trades in full against the venue's
/// latest quote, or expires.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Model)]
#[serde(rename_all = "camelCase")]
pub struct OrderRequest {
    /// The listed symbol, such as `"ACME"`.
    #[bimetal(min_length = 1, max_length = 12)]
    pub symbol: String,
    pub side: Side,
    /// The units to trade.
    #[bimetal(ge = 1)]
    pub quantity: u64,
    /// The worst price per unit to accept: the most a buy pays, or the least
    /// a sell takes. Without one, the order takes the quote.
    pub limit: Option<u64>,
}

/// How an order ended.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Model)]
#[serde(
    tag = "status",
    rename_all = "snake_case",
    rename_all_fields = "camelCase"
)]
pub enum OrderState {
    /// The order traded in full at `price` per unit, against the quote
    /// numbered `sequence`.
    Filled { price: u64, sequence: u64 },
    /// The quote was worse than the order's limit, so nothing traded.
    Expired { quote: u64 },
}

/// An order that the venue accepted, and its outcome.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Model)]
pub struct Order {
    /// Numbers count up from 1, in the order the venue accepted the orders.
    pub id: u64,
    pub request: OrderRequest,
    pub state: OrderState,
}

/// The best prices of one symbol, which the venue publishes at an interval.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Model)]
pub struct Quote {
    /// The venue numbers its quotes from 0, once per interval; every symbol's
    /// quote of one interval has the same number.
    pub sequence: u64,
    pub symbol: String,
    /// What a sell receives per unit.
    pub bid: u64,
    /// What a buy pays per unit, one cent above the bid.
    pub ask: u64,
}

/// The account and the latest quotes at one moment.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Model)]
pub struct Snapshot {
    pub cash: u64,
    /// The units held of each symbol, without the symbols that none are held of.
    pub positions: BTreeMap<String, u64>,
    /// How many orders the venue accepted, filled or expired.
    pub orders: u64,
    /// The latest quote of each listed symbol.
    pub quotes: Vec<Quote>,
}

/// Why the venue refused an order, or the account a withdrawal.
#[derive(Debug, thiserror::Error, bimetal::Error)]
pub enum OrderError {
    /// The venue does not list the symbol.
    #[error("`{symbol}` is not listed")]
    UnknownSymbol { symbol: String },
    /// A buy or a withdrawal needs more cash than the account has.
    #[error("{needed} cents are needed, but the account has {available}")]
    InsufficientBalance { needed: u64, available: u64 },
    /// A sell asks for more units than the account holds.
    #[error("the order sells {requested} of `{symbol}`, but the account holds {held}")]
    InsufficientPosition {
        symbol: String,
        requested: u64,
        held: u64,
    },
    /// The order would take the cash or a position past `u64::MAX`.
    #[error("the order would overflow the account")]
    Overflow,
}

/// The cost of `quantity` units at `price` cents each, for sizing an order.
///
/// # Panics
///
/// Panics when the cost does not fit in a `u64`. The engine checks its own
/// products, and refuses such an order instead.
#[bimetal::export]
#[must_use]
pub fn notional(price: u64, quantity: u64) -> u64 {
    price
        .checked_mul(quantity)
        .unwrap_or_else(|| panic!("{quantity} units at {price} cents overflow a u64"))
}
