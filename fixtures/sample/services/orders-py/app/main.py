"""HTTP surface for the orders service."""

from fastapi import FastAPI

from app.service import OrderService

app = FastAPI()
service = OrderService()


@app.get("/health")
def health():
    return {"status": "ok"}


@app.post("/orders")
def create_order(payload: dict):
    return service.create_order(payload)


@app.delete("/orders/{order_id}")
def cancel_order(order_id: int):
    return service.cancel_order(order_id)
