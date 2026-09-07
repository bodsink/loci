package main

import (
	"encoding/json"
	"net/http"
)

func healthHandler(w http.ResponseWriter, r *http.Request) {
	json.NewEncoder(w).Encode(map[string]string{"status": "ok"})
}

func stockHandler(w http.ResponseWriter, r *http.Request) {
	sku := r.URL.Query().Get("sku")
	stock := fetchInventory(sku)
	json.NewEncoder(w).Encode(stock)
}

func reserveHandler(w http.ResponseWriter, r *http.Request) {
	sku := r.URL.Query().Get("sku")
	if !isAvailable(sku, 1) {
		http.Error(w, "out of stock", http.StatusConflict)
		return
	}
	w.WriteHeader(http.StatusNoContent)
}
