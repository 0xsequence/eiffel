package main

import (
	"fmt"
	"net/http"
	"os"
	"strconv"
	"time"

	"github.com/mdlayher/vsock"
)

func main() {
	port, _ := strconv.Atoi(os.Getenv("PORT"))
	if port == 0 {
		port = 5000
	}

	l, err := vsock.Listen(uint32(port), nil)
	if err != nil {
		panic(err)
	}

	err = http.Serve(l, http.HandlerFunc(func(w http.ResponseWriter, r *http.Request) {
		w.Write([]byte(fmt.Sprintf("Hello from the enclave! The current time is %s", time.Now().String())))
		w.WriteHeader(http.StatusOK)
	}))
	if err != nil {
		panic(err)
	}
}
