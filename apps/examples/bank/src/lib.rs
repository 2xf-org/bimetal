//! A bank account that Python and TypeScript keep in Rust, and a bank whose
//! transfers settle one at a time on a task of its own. Amounts are cents.

mod ledger;

use std::collections::BTreeMap;
use std::sync::{Arc, Mutex, MutexGuard};
use std::time::Duration;

use bimetal::Model;
use bimetal::arrow::Table;
use serde::{Deserialize, Serialize};
use tokio::sync::broadcast::error::RecvError;
use tokio::sync::{broadcast, mpsc, oneshot};

use crate::ledger::Ledger;

#[derive(Debug, thiserror::Error, bimetal::Error)]
pub enum AccountError {
    #[error("insufficient funds")]
    InsufficientFunds,
}

#[derive(Default)]
pub struct Account {
    balance: u32,
}

#[bimetal::export]
impl Account {
    #[bimetal(constructor)]
    pub fn new() -> Self {
        Self::default()
    }

    pub fn deposit(&mut self, amount: u32) {
        self.balance += amount;
    }

    pub fn withdraw(&mut self, amount: u32) -> Result<(), AccountError> {
        if amount > self.balance {
            return Err(AccountError::InsufficientFunds);
        }
        self.balance -= amount;
        Ok(())
    }

    pub fn balance(&self) -> u32 {
        self.balance
    }
}

/// How long a withdrawal takes to settle.
const SETTLEMENT: Duration = Duration::from_millis(20);

/// The transfers that may wait for the settlement task before `transfer`
/// waits to send more.
const QUEUE: usize = 256;

/// The receipts that a slow `receipts` call may fall behind by before it
/// skips some.
const RECEIPT_BUFFER: usize = 1024;

/// The ledger's methods check their arithmetic, so no code panics with it
/// locked.
const UNPOISONED: &str = "no code panics while holding the ledger";

/// An account that a bank opens with.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Model)]
pub struct Opening {
    #[bimetal(min_length = 1, max_length = 32)]
    pub owner: String,
    pub balance: u64,
}

/// Money to move from one account to another.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Model)]
pub struct Transfer {
    pub sender: String,
    pub recipient: String,
    #[bimetal(ge = 1)]
    pub amount: u64,
}

/// A transfer that settled. Ids count up from 1, in the order of settlement.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Model)]
pub struct Receipt {
    pub id: u64,
    pub transfer: Transfer,
}

/// Every balance, and how many transfers settled, at one moment.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Model)]
pub struct Statement {
    pub balances: BTreeMap<String, u64>,
    pub transfers: u64,
}

/// Why a bank could not open.
#[derive(Debug, thiserror::Error, bimetal::Error)]
pub enum OpenError {
    #[error("`{owner}` has two accounts")]
    DuplicateOwner { owner: String },
}

/// Why a transfer or a withdrawal failed.
#[derive(Debug, thiserror::Error, bimetal::Error)]
pub enum TransferError {
    #[error("`{owner}` has no account")]
    UnknownAccount { owner: String },
    #[error("{needed} cents are needed, but the account has {available}")]
    InsufficientFunds { needed: u64, available: u64 },
    #[error("the transfer would take the balance past the largest amount")]
    Overflow,
}

/// The interest on `balance` at `percent`, rounded down. Panics when
/// `balance` times `percent` does not fit in a `u64`.
#[bimetal::export]
#[must_use]
pub fn interest(balance: u64, percent: u64) -> u64 {
    balance
        .checked_mul(percent)
        .unwrap_or_else(|| panic!("{balance} cents at {percent}% overflow a u64"))
        / 100
}

/// A transfer on its way to the settlement task, and where its outcome goes.
struct Request {
    transfer: Transfer,
    outcome: oneshot::Sender<Result<Receipt, TransferError>>,
}

/// A bank of accounts. A task settles its transfers one at a time, as they
/// arrive, and stops when the bank closes.
pub struct Bank {
    requests: mpsc::Sender<Request>,
    receipts: broadcast::Sender<Receipt>,
    ledger: Arc<Mutex<Ledger>>,
}

#[bimetal::export]
impl Bank {
    #[bimetal(constructor)]
    pub fn new(accounts: Vec<Opening>) -> Result<Self, OpenError> {
        let ledger = Arc::new(Mutex::new(Ledger::open(accounts)?));
        let (requests, mut received) = mpsc::channel::<Request>(QUEUE);
        let (receipts, _) = broadcast::channel(RECEIPT_BUFFER);
        let settled = receipts.clone();
        let settling = Arc::clone(&ledger);
        tokio::spawn(async move {
            while let Some(Request { transfer, outcome }) = received.recv().await {
                let result = settling.lock().expect(UNPOISONED).settle(transfer);
                if let Ok(receipt) = &result {
                    let _ = settled.send(receipt.clone());
                }
                // A caller that stopped waiting, as a cancelled Python task
                // does, has dropped its end.
                let _ = outcome.send(result);
            }
        });
        Ok(Self {
            requests,
            receipts,
            ledger,
        })
    }

    /// Send a transfer to the settlement task, and wait until it settles.
    pub async fn transfer(&self, transfer: Transfer) -> Result<Receipt, TransferError> {
        let (outcome, received) = oneshot::channel();
        self.requests
            .send(Request { transfer, outcome })
            .await
            .expect("the settlement task runs while the bank is open");
        received
            .await
            .expect("the settlement task answers every transfer it receives")
    }

    /// Wait for the next `count` transfers that settle. A call that falls
    /// more than 1,024 receipts behind skips the oldest.
    pub async fn receipts(&self, count: u32) -> Vec<Receipt> {
        let mut receiver = self.receipts.subscribe();
        let mut receipts = Vec::new();
        while receipts.len() < count as usize {
            match receiver.recv().await {
                Ok(receipt) => receipts.push(receipt),
                Err(RecvError::Lagged(_)) => {}
                Err(RecvError::Closed) => unreachable!("the bank holds a sender of receipts"),
            }
        }
        receipts
    }

    /// Take `amount` out of an account once it settles, and return what is
    /// left. The withdrawal holds the bank alone until then, so no transfer
    /// spends what it checked.
    pub async fn withdraw(&mut self, owner: String, amount: u64) -> Result<u64, TransferError> {
        self.locked().check(&owner, amount)?;
        tokio::time::sleep(SETTLEMENT).await;
        self.locked().withdraw(&owner, amount)
    }

    #[must_use]
    pub fn statement(&self) -> Statement {
        self.locked().statement()
    }

    /// Every settled transfer, oldest first: `id`, `sender`, `recipient` and
    /// `amount`.
    #[bimetal(returns(arrow))]
    #[must_use]
    pub fn ledger(&self) -> Table {
        self.locked().table()
    }
}

impl Bank {
    fn locked(&self) -> MutexGuard<'_, Ledger> {
        self.ledger.lock().expect(UNPOISONED)
    }
}
