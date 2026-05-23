BINARY := target/release/quran-tui
INSTALL_DIR := $(HOME)/.local/bin

.PHONY: build install reinstall

build:
	cargo build --release

install: build
	@mkdir -p $(INSTALL_DIR)
	install -m 755 $(BINARY) $(INSTALL_DIR)/quran-tui

reinstall:
	cargo clean
	cargo build --release
	@mkdir -p $(INSTALL_DIR)
	install -m 755 $(BINARY) $(INSTALL_DIR)/quran-tui

.DEFAULT_GOAL := build
