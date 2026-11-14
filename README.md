# Bimetal

Compile your Rust code into typed Python (`.abi3`) or TypeScript (`.wasm`/`.node`) packages.

[Tutorial](docs/tutorial.md) · [Documentation](docs/README.md) · [White paper](docs/paper/bimetal.pdf) · [Examples](#examples)

## How it works

Write your API in Rust and mark what Python and TypeScript may call. `bimetal build` turns it into a typed Python package and a typed npm package. The Python package runs your Rust as a native extension. The npm package runs it as WebAssembly, in Node.js and browsers, or as a native Node.js addon.

Field rules, error codes and docs come from the Rust, so they're the same in every package. You commit the generated files, so every API change shows up in review, and `bimetal check` fails CI when they no longer match the Rust.

## Getting started

Installing:

```bash
cargo install --locked bimetal-cli
```

Using:

```bash
bimetal new bank
cd bank
bimetal build
```

Requires Rust 1.94 with the Wasm target, Python 3.11 and Node.js 20.16, as [the requirements](docs/reference/limits.md#requirements) detail. Each [release](https://github.com/2xf-org/bimetal/releases) also has the command built for macOS, Linux and Windows, and the [tutorial](docs/tutorial.md) goes through each step.

### Supported

| Rust | Python | TypeScript |
| --- | --- | --- |
| [A struct or enum with `Model`](docs/guides/models.md) | A Pydantic model or `StrEnum` | An interface or string union |
| [An error type](docs/guides/errors.md) | An exception with a `code` | An `Error` with a `code` |
| [An `impl` block](docs/guides/resources.md) | A class, with `with` and `for` | A class, with `using` and `for...of` |
| [An `async fn`](docs/guides/async.md) | A coroutine | A `Promise` |
| [A `bimetal::Callback`](docs/guides/callbacks.md) | Any callable | Any function |
| [Bytes and numeric buffers](docs/guides/bytes.md) | `bytes` and NumPy arrays | `Uint8Array` and typed arrays |
| [Arrow values](docs/guides/arrow.md) | pyarrow, Polars or DuckDB, without a copy | apache-arrow |

## Example

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

Each call below runs the Rust natively and converts each value once, with no JSON in between. In Python, Arrow tables and data frames cross without a copy.

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

## Documentation

- [Tutorial](docs/tutorial.md): the account above, then a bank, from an empty folder.
- [Concepts and guides](docs/README.md): every kind of export, each host, and publishing.
- [Internals](docs/README.md#internals) and [testing](docs/internals/testing.md): for changing Bimetal itself.

### Examples

| Example | Shows |
| --- | --- |
| [`bank`](apps/examples/bank) | The account above, and a bank on a Tokio task, on the Node.js addon |
| [`showcase`](apps/examples/showcase) | Every kind of export, across two packages |
| [`streams`](apps/examples/streams) | Arrow streams through pyarrow, DuckDB and Polars, in Python |
| [`trials`](apps/trials) | DataFusion, Oxc, Polars, Tantivy and Typst, with [results](apps/trials/README.md) |

## White Paper

[Bimetal: One Rust API for Python, Node.js and the Browser](docs/paper/bimetal.pdf)

```bibtex
@techreport{faria2026bimetal,
  author = {Felipe Faria},
  title  = {Bimetal: One Rust API for Python, Node.js and the Browser},
  type   = {White paper},
  year   = {2026},
  month  = nov,
  url    = {https://github.com/2xf-org/bimetal/blob/main/docs/paper/bimetal.pdf}
}
```
