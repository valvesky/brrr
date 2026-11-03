CC ?= cc
PYTHON ?= /usr/bin/python3
NODE ?= node
PKGS = gio-2.0 gio-unix-2.0
CFLAGS ?= -O2 -Wall -Wextra -Wpedantic
CPPFLAGS += $(shell pkg-config --cflags $(PKGS))
LDLIBS += $(shell pkg-config --libs $(PKGS))

SOURCES = omapower.c config.c process-watch.c service-watch.c
HEADERS = omapower.h json-util.h vendor/json.h

# OMAPOWER_OUT (environment) redirects the binary, e.g. out of a watched plugin directory.
omapower: $(SOURCES) $(HEADERS)
	$(CC) $(CPPFLAGS) $(CFLAGS) $(LDFLAGS) -o "$${OMAPOWER_OUT:-$@}" omapower.c $(LDLIBS)

tests/test_config: tests/test_config.c config.c $(HEADERS)
	$(CC) $(CPPFLAGS) $(CFLAGS) $(LDFLAGS) -o $@ $< config.c $(LDLIBS)

tests/test_process: tests/test_process.c process-watch.c $(HEADERS)
	$(CC) $(CPPFLAGS) $(CFLAGS) $(LDFLAGS) -o $@ $< $(LDLIBS)

.PHONY: test clean
test: omapower tests/test_config tests/test_process
	./tests/test_config
	./tests/test_process
	sh tests/test_edit.sh
	$(PYTHON) tests/test_json.py
	$(NODE) tests/test_whitelist.js
	$(PYTHON) tests/test_setup.py
	dbus-run-session -- $(PYTHON) tests/test_daemon.py ./omapower

clean:
	rm -f omapower tests/test_config tests/test_process
