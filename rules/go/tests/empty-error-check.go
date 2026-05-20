package test

// Should trigger:
func bad() (int, error) {
	err := doSomething()
	if err != nil {
		return 0, nil
	}
	return 1, nil
}

// Should NOT trigger:
func good() (int, error) {
	err := doSomething()
	if err != nil {
		return 0, err
	}
	return 1, nil
}

func doSomething() error { return nil }
