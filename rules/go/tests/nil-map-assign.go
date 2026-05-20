package test

// Should trigger:
func bad() {
	var m map[string]int
	m["key"] = 1
}

// Should NOT trigger:
func good() {
	m := make(map[string]int)
	m["key"] = 1
}
