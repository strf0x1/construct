"""The package's one exception type."""


class InventoryError(Exception):
    """A business-rule violation, with a stable code for support tooling."""

    def __init__(self, code, message):
        super().__init__(f"{code}: {message}")
        self.code = code
