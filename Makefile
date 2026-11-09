# Omapower is a Rust program; this Makefile builds the binary for this machine
# and runs every test. Cross-platform release builds: scripts/build-release.sh.
CARGO ?= cargo
CC ?= cc
PYTHON ?= /usr/bin/python3
NODE ?= node

.PHONY: omapower test clean
omapower:
	$(CARGO) build --release
	cp target/release/omapower omapower

tests/thread-group: tests/thread-group.c
	$(CC) -O2 -Wall -Wextra -pthread -o $@ $<

test: omapower tests/thread-group
	$(CARGO) test --release
	sh tests/test_edit.sh
	$(PYTHON) tests/test_json.py
	$(NODE) tests/test_whitelist.js
	sh tests/test_retire.sh
	dbus-run-session -- $(PYTHON) tests/test_daemon.py ./omapower

clean:
	$(CARGO) clean
	rm -f omapower tests/thread-group
