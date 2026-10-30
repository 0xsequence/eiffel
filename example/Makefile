TOP := $(shell dirname $(realpath $(lastword $(MAKEFILE_LIST))))

.PHONY: eif
eif:
	mkdir -p bin
	docker build --platform linux/amd64 $(if $(EIFFEL_IMAGE),--build-arg EIFFEL_IMAGE=$(EIFFEL_IMAGE)) -t eiffel-example-builder .
	docker run --platform linux/amd64 -v $(TOP)/bin:/out eiffel-example-builder
