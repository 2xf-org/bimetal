"""
Move money between the accounts of a bank from Python.
"""

import asyncio

from bank import Bank, Opening, Transfer, TransferError


async def main() -> None:
    """
    Open a bank, settle one transfer, and refuse one that the sender cannot pay.
    """
    with Bank([Opening(owner="Ada", balance=100), Opening(owner="Grace", balance=0)]) as bank:
        receipt = await bank.transfer(Transfer(sender="Ada", recipient="Grace", amount=30))
        print(receipt.id, bank.statement().balances)  # 1 {'Ada': 70, 'Grace': 30}

        try:
            await bank.transfer(Transfer(sender="Grace", recipient="Ada", amount=500))
        except TransferError as error:
            print(error.code)  # insufficient_funds


asyncio.run(main())
