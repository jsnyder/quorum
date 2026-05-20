package test

import "net/http"

// Should trigger:
func bad() {
	http.ListenAndServe(":8080", nil)
}

// Should NOT trigger:
func good() {
	http.ListenAndServe("127.0.0.1:8080", nil)
}
