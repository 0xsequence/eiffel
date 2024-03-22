package main

import (
	"context"
	"io"
	"log"
	"os"
	"os/exec"

	"golang.org/x/sync/errgroup"
)

func main() {
	ctx, cancel := context.WithCancel(context.Background())
	defer cancel()
	wg, ctx := errgroup.WithContext(ctx)

	commands := []*exec.Cmd{exec.CommandContext(ctx, "/sbin/chronyd", "-d")}
	for _, arg := range os.Args[1:] {
		commands = append(commands, exec.CommandContext(ctx, arg))
	}

	for _, cmd := range commands {
		cmd := cmd

		stdout, err := cmd.StdoutPipe()
		if err != nil {
			log.Fatalf("Error creating stdout pipe: %s", err)
		}
		stderr, err := cmd.StderrPipe()
		if err != nil {
			log.Fatalf("Error creating stderr pipe: %s", err)
		}

		if err := cmd.Start(); err != nil {
			log.Fatalf("Error starting command: %s", err)
		}

		wg.Go(func() error {
			_, err := io.Copy(os.Stdout, stdout)
			return err
		})
		wg.Go(func() error {
			_, err := io.Copy(os.Stderr, stderr)
			return err
		})

		wg.Go(func() error {
			log.Printf("Started %s\n", cmd.Path)
			return cmd.Wait()
		})
	}

	if err := wg.Wait(); err != nil {
		log.Fatal(err.Error())
	}
}
