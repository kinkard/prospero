MAKEFLAGS += --warn-undefined-variables
SHELL := bash
.SHELLFLAGS := -eu -o pipefail -c

NAME := $(shell git remote get-url origin | awk -F: '{print $$2}' | sed 's/\.git$$//')
REVISION := $(shell git rev-parse HEAD)

docker:
	docker build . --tag $(NAME):$(REVISION)
	docker tag $(NAME):$(REVISION) $(NAME):latest
