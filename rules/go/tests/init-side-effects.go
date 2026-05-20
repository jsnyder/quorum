package test

import (
	"net/http"
	"os"
)

// Should trigger:
func init() {
	http.HandleFunc("/", handler)
	f, _ := os.Open("config.json")
	_ = f
}

// Should NOT trigger:
func init() {
	defaultValue = 42
}

func handler(w http.ResponseWriter, r *http.Request) {}
var defaultValue int
