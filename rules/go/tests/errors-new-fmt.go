package test

import (
	"errors"
	"fmt"
)

// Should trigger:
func bad(err error) error {
	return errors.New("prefix: " + err.Error())
}

// Should NOT trigger:
func good(err error) error {
	return fmt.Errorf("prefix: %w", err)
}
