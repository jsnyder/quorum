package test

import "sync"

// Should trigger:
func bad(wg *sync.WaitGroup) {
	go func() {
		wg.Add(1)
		defer wg.Done()
	}()
}

// Should NOT trigger:
func good(wg *sync.WaitGroup) {
	wg.Add(1)
	go func() {
		defer wg.Done()
	}()
}
