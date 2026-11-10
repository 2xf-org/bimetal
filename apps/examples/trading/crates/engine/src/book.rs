//! The account and the latest quotes, which the venue task changes and the
//! engine's synchronous calls read. Every method runs under the engine's lock,
//! which a panic would poison, so each sum and product is checked.

use std::collections::BTreeMap;
use std::sync::Arc;

use bimetal::arrow::array::{StringArray, UInt64Array};
use bimetal::arrow::{ArrayRef, RecordBatch, Table};
use trading_core::{Order, OrderError, OrderRequest, OrderState, Quote, Side, Snapshot};

/// One order's trade.
struct Fill {
    order_id: u64,
    symbol: String,
    side: Side,
    quantity: u64,
    price: u64,
    sequence: u64,
}

/// The account and the latest quotes.
pub(crate) struct Book {
    quotes: Vec<Quote>,
    cash: u64,
    positions: BTreeMap<String, u64>,
    orders: u64,
    fills: Vec<Fill>,
}

impl Book {
    /// An account with `cash` and no quotes yet.
    pub(crate) fn new(cash: u64) -> Self {
        Self {
            quotes: Vec::new(),
            cash,
            positions: BTreeMap::new(),
            orders: 0,
            fills: Vec::new(),
        }
    }

    /// Replace the latest quotes.
    pub(crate) fn quote(&mut self, quotes: &[Quote]) {
        self.quotes.clear();
        self.quotes.extend_from_slice(quotes);
    }

    /// Execute an order against the latest quote of its symbol.
    pub(crate) fn execute(&mut self, request: OrderRequest) -> Result<Order, OrderError> {
        let Some(quote) = self
            .quotes
            .iter()
            .find(|quote| quote.symbol == request.symbol)
        else {
            return Err(OrderError::UnknownSymbol {
                symbol: request.symbol,
            });
        };
        let sequence = quote.sequence;
        let price = match request.side {
            Side::Buy => quote.ask,
            Side::Sell => quote.bid,
        };
        let marketable = match (request.side, request.limit) {
            (_, None) => true,
            (Side::Buy, Some(limit)) => price <= limit,
            (Side::Sell, Some(limit)) => price >= limit,
        };
        let state = if marketable {
            match request.side {
                Side::Buy => self.buy(&request.symbol, request.quantity, price)?,
                Side::Sell => self.sell(&request.symbol, request.quantity, price)?,
            }
            OrderState::Filled { price, sequence }
        } else {
            OrderState::Expired { quote: price }
        };
        self.orders += 1;
        if matches!(state, OrderState::Filled { .. }) {
            self.fills.push(Fill {
                order_id: self.orders,
                symbol: request.symbol.clone(),
                side: request.side,
                quantity: request.quantity,
                price,
                sequence,
            });
        }
        Ok(Order {
            id: self.orders,
            request,
            state,
        })
    }

    /// Fail unless the account has `amount` in cash.
    pub(crate) fn check_cash(&self, amount: u64) -> Result<(), OrderError> {
        if amount > self.cash {
            return Err(OrderError::InsufficientBalance {
                needed: amount,
                available: self.cash,
            });
        }
        Ok(())
    }

    /// Take `amount` out of the cash, and return what is left.
    pub(crate) fn withdraw(&mut self, amount: u64) -> Result<u64, OrderError> {
        self.check_cash(amount)?;
        self.cash -= amount;
        Ok(self.cash)
    }

    pub(crate) fn snapshot(&self) -> Snapshot {
        Snapshot {
            cash: self.cash,
            positions: self.positions.clone(),
            orders: self.orders,
            quotes: self.quotes.clone(),
        }
    }

    /// Every fill, oldest first, with one column per field, none of them
    /// nullable.
    pub(crate) fn fills(&self) -> Table {
        let numbers = |field: fn(&Fill) -> u64| -> ArrayRef {
            Arc::new(UInt64Array::from_iter_values(self.fills.iter().map(field)))
        };
        let texts = |field: fn(&Fill) -> &str| -> ArrayRef {
            Arc::new(StringArray::from_iter_values(self.fills.iter().map(field)))
        };
        let batch = RecordBatch::try_from_iter([
            ("order_id", numbers(|fill| fill.order_id)),
            ("symbol", texts(|fill| &fill.symbol)),
            (
                "side",
                texts(|fill| match fill.side {
                    Side::Buy => "buy",
                    Side::Sell => "sell",
                }),
            ),
            ("quantity", numbers(|fill| fill.quantity)),
            ("price", numbers(|fill| fill.price)),
            ("sequence", numbers(|fill| fill.sequence)),
        ])
        .expect("every column has a row per fill");
        Table::try_new(batch.schema(), vec![batch]).expect("the batch has the table's schema")
    }

    fn buy(&mut self, symbol: &str, quantity: u64, price: u64) -> Result<(), OrderError> {
        let available = self.cash;
        let cost = price
            .checked_mul(quantity)
            .filter(|cost| *cost <= available)
            .ok_or(OrderError::InsufficientBalance {
                needed: price.saturating_mul(quantity),
                available,
            })?;
        let held = self.positions.get(symbol).copied().unwrap_or(0);
        let position = held.checked_add(quantity).ok_or(OrderError::Overflow)?;
        self.cash = available - cost;
        self.positions.insert(symbol.to_owned(), position);
        Ok(())
    }

    fn sell(&mut self, symbol: &str, quantity: u64, price: u64) -> Result<(), OrderError> {
        let held = self.positions.get(symbol).copied().unwrap_or(0);
        let Some(position) = held.checked_sub(quantity) else {
            return Err(OrderError::InsufficientPosition {
                symbol: symbol.to_owned(),
                requested: quantity,
                held,
            });
        };
        self.cash = price
            .checked_mul(quantity)
            .and_then(|proceeds| self.cash.checked_add(proceeds))
            .ok_or(OrderError::Overflow)?;
        if position == 0 {
            self.positions.remove(symbol);
        } else {
            self.positions.insert(symbol.to_owned(), position);
        }
        Ok(())
    }
}
