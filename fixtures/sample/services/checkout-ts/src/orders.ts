export interface OrderRequest {
  sku: string;
  quantity: number;
}

export interface OrderResponse {
  id: number;
  total: number;
}

const ORDERS_BASE_URL = process.env.ORDERS_BASE_URL ?? "http://localhost:8000";

export function buildOrderRequest(sku: string, quantity: number): OrderRequest {
  return { sku, quantity };
}

export function orderEndpoint(path: string): string {
  return `${ORDERS_BASE_URL}${path}`;
}

export async function submitOrder(request: OrderRequest): Promise<OrderResponse> {
  const url = orderEndpoint("/orders");
  const response = await fetch(url, {
    method: "POST",
    body: JSON.stringify(request),
  });
  return response.json() as Promise<OrderResponse>;
}
