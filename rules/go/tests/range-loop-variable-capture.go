package test

import "fmt"

// Should trigger:
func bad(items []string) {
	for _, item := range items {
		go func() {
			fmt.Println(item)
		}()
	}
}

// Should NOT trigger:
func good(items []string) {
	for _, item := range items {
		item := item
		go func() {
			fmt.Println(item)
		}()
	}
}
