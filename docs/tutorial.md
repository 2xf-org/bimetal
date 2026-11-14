# Tutorial

This tutorial writes a bank account in Rust, a class whose balance stays in Rust and an error that callers branch on, and calls it from Python and TypeScript. Then it adds a bank that moves money between accounts, with models, a field rule and async calls. Everything here is in [`apps/examples/bank`](../apps/examples/bank), whose tests run these programs and check what they print.

You need Rust 1.94 or newer with the Wasm target (`rustup target add wasm32-unknown-unknown`), Python 3.11 or newer, and Node.js 20.16 or newer. [Limits and requirements](reference/limits.md) has the details.

## Install the command

```bash
cargo install --locked bimetal-cli
```

Each [release](https://github.com/2xf-org/bimetal/releases) also has the command built for macOS, Linux and Windows. The command and the `bimetal` crate your application depends on must have the same version, and Bimetal stops with an error if they differ.

## Create the application

```bash
bimetal new bank
```

That writes a Cargo package and a project for each host:

```text
bank/
├── Cargo.toml
├── bimetal.toml           turns on each host
├── src/lib.rs           your API
├── python/              a Python project, with pyproject.toml
└── typescript/          an npm project, with package.json
```

All of these files are yours. Bimetal adds a package inside each project when it builds.

## Write an account

Replace `src/lib.rs`. `#[bimetal::export]` on an `impl` block makes a class whose value stays in Rust between calls, and `#[bimetal(constructor)]` marks what creates it. An error is an enum that derives `bimetal::Error`, with `thiserror` in `Cargo.toml` for its message:

```rust
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
```

## Build

```bash
bimetal build
```

The first build compiles every dependency for each host and takes a while. Later builds reuse them, and a build where nothing changed compiles nothing:

```text
Compiling bank: contract discovery, WebAssembly module, Python extension
  Compiled the contract discovery in …s
  Compiled the WebAssembly module in …s
  Compiled the Python extension in …s
Built bank 0.1.0.
  Python package bank in python
  npm package bank in typescript
```

Bimetal compiled your Rust once for each host, read the API from the compiled code, and wrote a package into each project. [Packages and projects](concepts/packages.md) shows what it wrote.

## Call it from Python

`python/` is an ordinary project, so `uv run --project python python` or `pip install ./python` both work:

```python
from bank import Account, AccountError

account = Account()
account.deposit(100)
account.withdraw(30)
print(account.balance())  # 70

try:
    account.withdraw(500)
except AccountError as error:
    print(error.code)  # insufficient_funds
```

Each variant of the error is a stable `code`, which is what you branch on. The package ships `py.typed` and passes `mypy --strict`, so your editor knows every type. The balance lives in Rust until the account is garbage-collected, or until `close()` or the end of a `with` block frees it.

## Call it from TypeScript

`typescript/` is an ordinary npm package, and the class has the same methods:

```ts
import { Account, AccountError } from "bank";

const account = new Account();
account.deposit(100);
account.withdraw(30);
console.log(account.balance()); // 70

try {
  account.withdraw(500);
} catch (error) {
  if (error instanceof AccountError) console.log(error.code); // insufficient_funds
}
```

By default the package runs your Rust as a WebAssembly module, so the same package works in Node.js and in a browser. A browser loads the module asynchronously, so a page first awaits `nativeReady`, as [Browsers](guides/browser.md) explains. `using account = new Account()` frees the Rust value when the block ends.

## Move money between accounts

A *model* is a type that crosses as data. Derive `Model` beside Serde's traits:

```rust
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
```

`#[bimetal(ge = 1)]` is a *field rule*, which every host enforces: Pydantic refuses a transfer of nothing where it is built, and Rust refuses it before your code runs. Doc comments become Python docstrings and TypeScript JSDoc. An error's variant can carry fields, which its message uses:

```rust
#[derive(Debug, thiserror::Error, bimetal::Error)]
pub enum TransferError {
    #[error("`{owner}` has no account")]
    UnknownAccount { owner: String },
    #[error("{needed} cents are needed, but the account has {available}")]
    InsufficientFunds { needed: u64, available: u64 },
    #[error("the transfer would take the balance past the largest amount")]
    Overflow,
}
```

The example's `Bank` keeps every balance, and a Tokio task settles its transfers one at a time, so `transfer` is an `async fn`:

```rust
#[bimetal::export]
impl Bank {
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
}
```

WebAssembly can't run Tokio's threads, so the example sets `backend = "node"` under `[typescript]` in `bimetal.toml`, and its npm package runs as a native Node.js addon. [Hosts](concepts/hosts.md) compares the two backends. In Python, `transfer` is a coroutine:

```python
async def main() -> None:
    with Bank([Opening(owner="Ada", balance=100), Opening(owner="Grace", balance=0)]) as bank:
        receipt = await bank.transfer(Transfer(sender="Ada", recipient="Grace", amount=30))
        print(receipt.id, bank.statement().balances)  # 1 {'Ada': 70, 'Grace': 30}

        try:
            await bank.transfer(Transfer(sender="Grace", recipient="Ada", amount=500))
        except TransferError as error:
            print(error.code)  # insufficient_funds
```

In TypeScript, it returns a `Promise`, and a model is a plain object. A `u64` is a `bigint`:

```ts
using bank = new Bank([
  { owner: "Ada", balance: 100n },
  { owner: "Grace", balance: 0n },
]);
const receipt = await bank.transfer({ sender: "Ada", recipient: "Grace", amount: 30n });
console.log(receipt.id, bank.statement().balances); // 1n { Ada: 70n, Grace: 30n }
```

The example's bank also streams receipts as they settle, holds itself alone through a withdrawal, and returns its ledger as an Arrow table. Its tests run a thousand transfers at once, from threads, a forked process and Node.js workers.

## Keep the packages current

Commit the generated files with the Rust change that caused them. CI then needs one line, which fails if anyone changed the Rust without rebuilding:

```bash
bimetal check
```

Don't commit the compiled extension or addon, since each platform builds its own. In a fresh clone, `bimetal build` compiles only those. [Keeping packages current](guides/current.md) has the rest.

## Where to go next

Read the [concepts](README.md) in order, and the [guides](README.md#guides) for whatever your API needs. [`apps/examples/showcase`](../apps/examples/showcase) uses every feature, and [`apps/trials/`](../apps/trials) wraps five large projects.
