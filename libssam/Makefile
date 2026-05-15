# Copyright 2025-2026 Hyundai Mobis Co., Ltd.
# SPDX-License-Identifier: Apache-2.0

all:

release-artifacts:
	@if [ ! -d "$(OUTPUT_DIR)" ]; then \
		echo "[ERROR] Given release output directory '$(OUTPUT_DIR)' does not exist!"; \
		exit 1; \
	fi
	@cp data/ssam.remocon.proto data/remocon-responses.schema.json $(OUTPUT_DIR)/

.PHONY: all release-artifacts
