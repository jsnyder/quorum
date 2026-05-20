package test

// Should trigger:
func bad(s string) {
	for i := 0; i < 100; i++ {
		_ = []byte(s)
	}
}

// Should NOT trigger:
func good(s string) {
	b := []byte(s)
	for i := 0; i < 100; i++ {
		_ = b
	}
}
