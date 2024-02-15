package main

import (
	"io"
	"log"
	"os"
	"os/exec"

	"golang.org/x/sync/errgroup"
)

func main() {
	commands := []*exec.Cmd{exec.Command("/sbin/chronyd", "-d")}
	for _, arg := range os.Args[1:] {
		commands = append(commands, exec.Command(arg))
	}

	var wg errgroup.Group
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
