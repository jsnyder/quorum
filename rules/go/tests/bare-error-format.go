package test

import "fmt"

// Should trigger:
func bad(err error) error {
	return fmt.Errorf("failed: %v", err)
}

// Should NOT trigger:
func good(err error) error {
	return fmt.Errorf("failed: %w", err)
}
