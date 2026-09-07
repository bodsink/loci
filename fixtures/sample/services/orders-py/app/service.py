"""Order business rules for the sample fixture."""

from app.repository import find_order, save_order


class OrderError(Exception):
    pass


def validate_order(payload):
    if not payload.get("sku"):
        raise OrderError("sku is required")
    return True


def price_order(payload):
    quantity = payload.get("quantity", 1)
    return quantity * 1000


class OrderService:
    def __init__(self, currency="IDR"):
        self.currency = currency

    def create_order(self, payload):
        validate_order(payload)
        total = price_order(payload)
        return save_order({"total": total, "currency": self.currency})

    def cancel_order(self, order_id):
        order = find_order(order_id)
        if order is None:
            raise OrderError("unknown order")
        order["status"] = "cancelled"
        return save_order(order)
