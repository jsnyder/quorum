package test

import "os"

// Should trigger:
func bad() {
	_, _ = os.Open("file.txt")
}

// Should NOT trigger:
func good() {
	f, err := os.Open("file.txt")
	if err != nil {
		return
	}
	_ = f.Close()
}
