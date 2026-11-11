# brrr is a Rust program; this Makefile builds the binary for this machine
# and runs every test. Cross-platform release builds: scripts/build-release.sh.
CARGO ?= cargo
CC ?= cc
PYTHON ?= /usr/bin/python3
NODE ?= node

.PHONY: brrr test clean
brrr:
	$(CARGO) build --release
	cp target/release/brrr brrr

tests/thread-group: tests/thread-group.c
	$(CC) -O2 -Wall -Wextra -pthread -o $@ $<

test: brrr tests/thread-group
	$(CARGO) test --release
	sh tests/test_edit.sh
	$(PYTHON) tests/test_json.py
	$(NODE) tests/test_whitelist.js
	sh tests/test_retire.sh
	dbus-run-session -- $(PYTHON) tests/test_daemon.py ./brrr

clean:
	$(CARGO) clean
	rm -f brrr tests/thread-group
