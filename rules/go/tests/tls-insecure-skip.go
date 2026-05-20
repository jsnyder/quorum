package test

import "crypto/tls"

// Should trigger:
func bad() *tls.Config {
	return &tls.Config{InsecureSkipVerify: true}
}

// Should NOT trigger:
func good() *tls.Config {
	return &tls.Config{InsecureSkipVerify: false}
}
