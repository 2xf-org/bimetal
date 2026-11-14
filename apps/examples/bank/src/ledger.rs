//! The balances and the settled transfers, which the settlement task changes
//! and the bank's synchronous calls read. Every method runs under the bank's
//! lock, which a panic would poison, so each sum is checked.

use std::collections::BTreeMap;
use std::collections::btree_map::Entry;
use std::sync::Arc;

use bimetal::arrow::array::{StringArray, UInt64Array};
use bimetal::arrow::{ArrayRef, RecordBatch, Table};

use crate::{OpenError, Opening, Receipt, Statement, Transfer, TransferError};

pub(crate) struct Ledger {
    balances: BTreeMap<String, u64>,
    receipts: Vec<Receipt>,
}

impl Ledger {
    pub(crate) fn open(accounts: Vec<Opening>) -> Result<Self, OpenError> {
        let mut balances = BTreeMap::new();
        for Opening { owner, balance } in accounts {
            match balances.entry(owner) {
                Entry::Vacant(account) => {
                    account.insert(balance);
                }
                Entry::Occupied(account) => {
                    return Err(OpenError::DuplicateOwner {
                        owner: account.key().clone(),
                    });
                }
            }
        }
        Ok(Self {
            balances,
            receipts: Vec::new(),
        })
    }

    /// Move the money of `transfer`, or change nothing and say why not.
    pub(crate) fn settle(&mut self, transfer: Transfer) -> Result<Receipt, TransferError> {
        let available = self.balance(&transfer.sender)?;
        let credited = self.balance(&transfer.recipient)?;
        let Some(left) = available.checked_sub(transfer.amount) else {
            return Err(TransferError::InsufficientFunds {
                needed: transfer.amount,
                available,
            });
        };
        if transfer.sender != transfer.recipient {
            let total = credited
                .checked_add(transfer.amount)
                .ok_or(TransferError::Overflow)?;
            self.balances.insert(transfer.sender.clone(), left);
            self.balances.insert(transfer.recipient.clone(), total);
        }
        let receipt = Receipt {
            id: self.receipts.len() as u64 + 1,
            transfer,
        };
        self.receipts.push(receipt.clone());
        Ok(receipt)
    }

    /// Fail unless `owner` has `amount`.
    pub(crate) fn check(&self, owner: &str, amount: u64) -> Result<(), TransferError> {
        let available = self.balance(owner)?;
        if amount > available {
            return Err(TransferError::InsufficientFunds {
                needed: amount,
                available,
            });
        }
        Ok(())
    }

    /// Take `amount` out of `owner`'s account, and return what is left.
    pub(crate) fn withdraw(&mut self, owner: &str, amount: u64) -> Result<u64, TransferError> {
        self.check(owner, amount)?;
        let left = self.balance(owner)? - amount;
        self.balances.insert(owner.to_owned(), left);
        Ok(left)
    }

    pub(crate) fn statement(&self) -> Statement {
        Statement {
            balances: self.balances.clone(),
            transfers: self.receipts.len() as u64,
        }
    }

    /// Every settled transfer, oldest first, with one column per field.
    pub(crate) fn table(&self) -> Table {
        let numbers = |field: fn(&Receipt) -> u64| -> ArrayRef {
            Arc::new(UInt64Array::from_iter_values(
                self.receipts.iter().map(field),
            ))
        };
        let texts = |field: fn(&Receipt) -> &str| -> ArrayRef {
            Arc::new(StringArray::from_iter_values(
                self.receipts.iter().map(field),
            ))
        };
        let batch = RecordBatch::try_from_iter([
            ("id", numbers(|receipt| receipt.id)),
            ("sender", texts(|receipt| &receipt.transfer.sender)),
            ("recipient", texts(|receipt| &receipt.transfer.recipient)),
            ("amount", numbers(|receipt| receipt.transfer.amount)),
        ])
        .expect("every column has a row per receipt");
        Table::try_new(batch.schema(), vec![batch]).expect("the batch has the table's schema")
    }

    fn balance(&self, owner: &str) -> Result<u64, TransferError> {
        self.balances
            .get(owner)
            .copied()
            .ok_or_else(|| TransferError::UnknownAccount {
                owner: owner.to_owned(),
            })
    }
}
