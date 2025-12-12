BINARY_NAME = nootnoot
INSTALL_BIN_DIR = /usr/local/bin
SYSTEMD_DIR = /etc/systemd/system
CONFIG_PATH = /etc/nootnoot.toml

.PHONY: all build install uninstall install-user uninstall-user

all: build

build:
	cargo build --release

install: build
	# Install binary
	sudo install -Dm755 target/release/$(BINARY_NAME) $(INSTALL_BIN_DIR)/$(BINARY_NAME)

	# Install config if not present
	if [ ! -f "$(CONFIG_PATH)" ]; then \
	  sudo install -Dm644 config/nootnoot.toml.example $(CONFIG_PATH); \
	fi

	# Install systemd unit
	sudo install -Dm644 packaging/systemd/nootnoot.service \
	  $(SYSTEMD_DIR)/nootnoot.service

	# Reload and enable/start
	sudo systemctl daemon-reload
	sudo systemctl enable --now nootnoot.service

uninstall:
	- sudo systemctl stop nootnoot.service
	- sudo systemctl disable nootnoot.service
	- sudo rm -f $(SYSTEMD_DIR)/nootnoot.service
	- sudo systemctl daemon-reload
	- sudo rm -f $(INSTALL_BIN_DIR)/$(BINARY_NAME)
	@echo "Config file left at $(CONFIG_PATH) (remove manually if you want)."

# User-mode systemd service (no root)
install-user: build
	install -Dm755 target/release/$(BINARY_NAME) $$HOME/.local/bin/$(BINARY_NAME)
	install -Dm644 config/nootnoot.toml.example $$HOME/.config/nootnoot.toml
	install -Dm644 packaging/systemd/nootnoot.service \
	  $$HOME/.config/systemd/user/nootnoot.service

	systemctl --user daemon-reload
	systemctl --user enable --now nootnoot.service

uninstall-user:
	- systemctl --user stop nootnoot.service
	- systemctl --user disable nootnoot.service || true
	- rm -f $$HOME/.config/systemd/user/nootnoot.service
	- systemctl --user daemon-reload
	- rm -f $$HOME/.local/bin/$(BINARY_NAME)
	@echo "Config file left at $$HOME/.config/nootnoot.toml"
