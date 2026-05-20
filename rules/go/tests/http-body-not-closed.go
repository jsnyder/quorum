package test

import "net/http"

// Should trigger:
func bad() {
	resp, err := http.Get("https://example.com")
	if err != nil {
		return
	}
	_ = resp
}

// Should NOT trigger:
func good() {
	resp, err := http.Get("https://example.com")
	if err != nil {
		return
	}
	defer resp.Body.Close()
}
