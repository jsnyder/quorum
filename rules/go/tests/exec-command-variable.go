package test

import "os/exec"

// Should trigger:
func bad(cmd string) {
	exec.Command(cmd, "arg1")
}

// Should NOT trigger:
func good() {
	exec.Command("ls", "-la")
}
