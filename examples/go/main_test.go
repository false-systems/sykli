package main

import "testing"

func TestAnswer(t *testing.T) {
	if got := answer(); got != 42 {
		t.Fatalf("answer = %d, want 42", got)
	}
}
