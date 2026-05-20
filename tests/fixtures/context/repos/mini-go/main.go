package main

import (
	"fmt"
	"net/http"
)

// Server handles HTTP requests.
type Server struct {
	port int
}

// NewServer creates a new Server instance.
func NewServer(port int) *Server {
	return &Server{port: port}
}

// Start begins listening for connections.
func (s *Server) Start() error {
	addr := fmt.Sprintf(":%d", s.port)
	return http.ListenAndServe(addr, nil)
}

func helper() {
	fmt.Println("unexported helper")
}
