from dataclasses import dataclass


@dataclass
class Entry:
    account: str
    amount: int
