"""In-memory persistence stub."""

_ORDERS = {}
_NEXT_ID = {"value": 1}


def save_order(order):
    order_id = order.get("id") or _allocate_id()
    order["id"] = order_id
    _ORDERS[order_id] = order
    return order


def find_order(order_id):
    return _ORDERS.get(order_id)


def _allocate_id():
    current = _NEXT_ID["value"]
    _NEXT_ID["value"] = current + 1
    return current
