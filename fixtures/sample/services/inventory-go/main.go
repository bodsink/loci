package main

import (
	"log"
	"net/http"
)

func registerRoutes() {
	http.HandleFunc("/health", healthHandler)
	http.HandleFunc("/stock", stockHandler)
	http.HandleFunc("/reserve", reserveHandler)
}

func main() {
	registerRoutes()
	log.Fatal(http.ListenAndServe(":8081", nil))
}
