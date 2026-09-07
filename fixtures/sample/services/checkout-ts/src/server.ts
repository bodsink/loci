import express from "express";

import { buildOrderRequest, submitOrder } from "./orders";

const app = express();

export async function checkoutHandler(req: any, res: any) {
  const request = buildOrderRequest(req.body.sku, req.body.quantity);
  const order = await submitOrder(request);
  res.json(order);
}

export function healthHandler(_req: any, res: any) {
  res.json({ status: "ok" });
}

app.get("/health", healthHandler);
app.post("/checkout", checkoutHandler);

export default app;
