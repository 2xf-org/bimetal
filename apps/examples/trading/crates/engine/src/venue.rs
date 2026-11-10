//! The venue session: one task that publishes the market's quotes on a timer,
//! and executes each order as it arrives.

use std::sync::{Arc, Mutex};
use std::time::Duration;

use tokio::sync::{broadcast, mpsc, oneshot};
use tokio::time::{self, Instant, MissedTickBehavior};
use trading_core::{Order, OrderError, OrderRequest, Quote};

use crate::UNPOISONED;
use crate::book::Book;
use crate::market::Market;

/// An order on its way to the venue, and where its outcome goes.
pub(crate) struct Submission {
    pub(crate) request: OrderRequest,
    pub(crate) outcome: oneshot::Sender<Result<Order, OrderError>>,
}

/// What the venue task owns, and shares with the engine.
pub(crate) struct Venue {
    pub(crate) market: Market,
    pub(crate) book: Arc<Mutex<Book>>,
    pub(crate) orders: mpsc::Receiver<Submission>,
    pub(crate) quotes: broadcast::Sender<Quote>,
    pub(crate) interval: Duration,
}

impl Venue {
    /// Publish the opening quotes, then report the session open on `opened`.
    /// From then on, publish quotes each interval and execute the orders
    /// between them, until the engine drops its end of `orders`.
    pub(crate) async fn run(mut self, opened: oneshot::Sender<()>) {
        self.publish();
        // A connect call that was cancelled no longer waits.
        let _ = opened.send(());
        let mut ticker = time::interval_at(Instant::now() + self.interval, self.interval);
        ticker.set_missed_tick_behavior(MissedTickBehavior::Delay);
        loop {
            tokio::select! {
                _ = ticker.tick() => {
                    self.market.step();
                    self.publish();
                }
                submission = self.orders.recv() => {
                    let Some(Submission { request, outcome }) = submission else {
                        return;
                    };
                    let result = self.book.lock().expect(UNPOISONED).execute(request);
                    // A caller that stopped waiting, as a cancelled Python task
                    // does, has dropped its end.
                    let _ = outcome.send(result);
                }
            }
        }
    }

    /// Make the market's quotes the book's latest, and send them to the
    /// callers that wait for quotes, if any.
    fn publish(&self) {
        let quotes = self.market.quotes();
        self.book.lock().expect(UNPOISONED).quote(quotes);
        for quote in quotes {
            let _ = self.quotes.send(quote.clone());
        }
    }
}
