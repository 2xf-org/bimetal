// Keep an account in Rust from TypeScript.

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
