"""
Keep an account in Rust from Python.
"""

from bank import Account, AccountError

account = Account()
account.deposit(100)
account.withdraw(30)
print(account.balance())  # 70

try:
    account.withdraw(500)
except AccountError as error:
    print(error.code)  # insufficient_funds
