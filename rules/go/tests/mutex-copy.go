package test

import "sync"

// Should trigger:
func bad(m sync.Mutex) {}

// Should NOT trigger:
func good(m *sync.Mutex) {}
