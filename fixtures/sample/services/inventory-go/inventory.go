package main

import "strings"

// Stock is the inventory level for one SKU.
type Stock struct {
	SKU      string
	Quantity int
}

var catalog = map[string]int{
	"widget": 12,
	"gizmo":  3,
}

func normaliseSKU(sku string) string {
	return strings.ToLower(strings.TrimSpace(sku))
}

func fetchInventory(sku string) Stock {
	key := normaliseSKU(sku)
	return Stock{SKU: key, Quantity: catalog[key]}
}

func isAvailable(sku string, wanted int) bool {
	stock := fetchInventory(sku)
	return stock.Quantity >= wanted
}
