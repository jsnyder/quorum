package test

import "sync"

// Should trigger:
var badPool = sync.Pool{
	New: func() interface{} {
		return make([]byte, 1024)
	},
}

// Should NOT trigger:
var goodPool = sync.Pool{
	New: func() *[]byte {
		b := make([]byte, 1024)
		return &b
	},
}
