package test

import "os"

// Should trigger:
func bad() {
	for i := 0; i < 10; i++ {
		f, _ := os.Open("file.txt")
		defer f.Close()
	}
}

// Should NOT trigger:
func good() {
	f, _ := os.Open("file.txt")
	defer f.Close()
}
