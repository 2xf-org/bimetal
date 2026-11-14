// Move money between the accounts of a bank from TypeScript.

import { Bank, TransferError } from "bank";

using bank = new Bank([
  { owner: "Ada", balance: 100n },
  { owner: "Grace", balance: 0n },
]);
const receipt = await bank.transfer({ sender: "Ada", recipient: "Grace", amount: 30n });
console.log(receipt.id, bank.statement().balances); // 1n { Ada: 70n, Grace: 30n }

try {
  await bank.transfer({ sender: "Grace", recipient: "Ada", amount: 500n });
} catch (error) {
  if (error instanceof TransferError) console.log(error.code); // insufficient_funds
}
