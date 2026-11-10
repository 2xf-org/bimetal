//! A trading engine against a simulated venue. The engine spawns a Tokio task
//! that publishes quotes on a timer and executes orders as they arrive. Hosts
//! await orders and quotes, and read snapshots and the fills table
//! synchronously. The models live in `trading-core`.

mod book;
mod market;
mod venue;

use std::sync::{Arc, Mutex, MutexGuard};
use std::time::Duration;

use bimetal::Model;
use bimetal::arrow::Table;
use serde::{Deserialize, Serialize};
use tokio::sync::broadcast::error::RecvError;
use tokio::sync::{broadcast, mpsc, oneshot};
use tokio::time;
use trading_core::{Order, OrderError, OrderRequest, Quote, Snapshot};

use crate::book::Book;
use crate::market::Market;
use crate::venue::{Submission, Venue};

/// How long a withdrawal takes to settle.
const SETTLEMENT: Duration = Duration::from_millis(20);

/// The orders that may wait for the venue before `submit` waits to send more.
const ORDER_QUEUE: usize = 256;

/// The quotes that a slow `quotes` call may fall behind by before it skips
/// some.
const QUOTE_BUFFER: usize = 1024;

/// The reason to expect when locking the book: its methods check their
/// arithmetic instead of panicking.
const UNPOISONED: &str = "no code panics while holding the book";

/// A symbol that the venue lists.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Model)]
pub struct Listing {
    #[bimetal(min_length = 1, max_length = 12)]
    pub symbol: String,
    /// The opening bid, in cents.
    #[bimetal(ge = 1)]
    pub price: u64,
}

/// How to open a session.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Model)]
#[serde(rename_all = "camelCase")]
pub struct EngineConfig {
    /// The cash that the account starts with, in cents.
    pub cash: u64,
    #[bimetal(min_length = 1, max_length = 64)]
    pub listings: Vec<Listing>,
    /// How often the venue publishes quotes, in milliseconds.
    #[bimetal(ge = 1)]
    pub quote_interval_ms: u32,
    /// The seed of the simulated prices, which repeat for the same seed. The
    /// operating system picks one when it is absent.
    pub seed: Option<u64>,
}

/// Why a session could not open.
#[derive(Debug, thiserror::Error, bimetal::Error)]
pub enum ConfigError {
    /// Two listings have the same symbol.
    #[error("`{symbol}` is listed twice")]
    DuplicateSymbol { symbol: String },
}

/// A session with the simulated venue, and the account that it trades.
pub struct Engine {
    orders: mpsc::Sender<Submission>,
    quotes: broadcast::Sender<Quote>,
    book: Arc<Mutex<Book>>,
}

#[bimetal::export]
impl Engine {
    /// Open a session: start the venue task, and wait until it has published
    /// the opening quotes. The task stops when the engine is dropped.
    #[bimetal(constructor)]
    pub async fn connect(config: EngineConfig) -> Result<Self, ConfigError> {
        let seed = match config.seed {
            Some(seed) => seed,
            None => getrandom::u64().expect("the operating system provides random numbers"),
        };
        let market = Market::open(&config.listings, seed)?;
        let book = Arc::new(Mutex::new(Book::new(config.cash)));
        let (orders, received) = mpsc::channel(ORDER_QUEUE);
        let (quotes, _) = broadcast::channel(QUOTE_BUFFER);
        let (opened, open) = oneshot::channel();
        let venue = Venue {
            market,
            book: Arc::clone(&book),
            orders: received,
            quotes: quotes.clone(),
            interval: Duration::from_millis(config.quote_interval_ms.into()),
        };
        tokio::spawn(venue.run(opened));
        open.await
            .expect("the venue task reports the session open before anything else");
        Ok(Self {
            orders,
            quotes,
            book,
        })
    }

    /// Send an order to the venue, and wait for its outcome. The venue
    /// executes orders one at a time, as they arrive, against its latest
    /// quote.
    ///
    /// # Panics
    ///
    /// Panics if the venue task has stopped, which only a bug in it can do
    /// while the engine is open.
    pub async fn submit(&self, order: OrderRequest) -> Result<Order, OrderError> {
        let (outcome, received) = oneshot::channel();
        self.orders
            .send(Submission {
                request: order,
                outcome,
            })
            .await
            .expect("the venue task runs while the engine is open");
        received
            .await
            .expect("the venue task answers every order it receives")
    }

    /// The account and the latest quotes, at one moment.
    #[must_use]
    pub fn snapshot(&self) -> Snapshot {
        self.book().snapshot()
    }

    /// Every fill so far, oldest first: `order_id`, `symbol`, `side`,
    /// `quantity`, `price`, and `sequence`, the number of the quote it traded
    /// against.
    #[bimetal(returns(arrow))]
    #[must_use]
    pub fn fills(&self) -> Table {
        self.book().fills()
    }

    /// Wait for the next `count` quotes that the venue publishes: one per
    /// listing each interval. A call that falls more than 1,024 quotes behind
    /// skips the oldest.
    pub async fn quotes(&self, count: u32) -> Vec<Quote> {
        let mut receiver = self.quotes.subscribe();
        let mut quotes = Vec::new();
        while quotes.len() < count as usize {
            match receiver.recv().await {
                Ok(quote) => quotes.push(quote),
                Err(RecvError::Lagged(_)) => {}
                Err(RecvError::Closed) => unreachable!("the engine holds a sender of quotes"),
            }
        }
        quotes
    }

    /// Withdraw cash, once the transfer settles, and return the cash left.
    /// No order runs until it has, so an order cannot spend the cash that the
    /// withdrawal checked.
    pub async fn withdraw(&mut self, amount: u64) -> Result<u64, OrderError> {
        self.book().check_cash(amount)?;
        time::sleep(SETTLEMENT).await;
        self.book().withdraw(amount)
    }
}

impl Engine {
    fn book(&self) -> MutexGuard<'_, Book> {
        self.book.lock().expect(UNPOISONED)
    }
}
