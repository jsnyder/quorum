package test

import (
	"errors"
	"fmt"
)

// Should trigger:
func bad() error {
	return errors.New(fmt.Sprintf("failed: %d", 42))
}

// Should NOT trigger:
func good() error {
	return fmt.Errorf("failed: %d", 42)
}
