# Copyright 2025-2026 Hyundai Mobis Co., Ltd.
# SPDX-License-Identifier: Apache-2.0

no_target:
	$(error "Error: You must specify a target to build.")

BINDGEN := $(shell which bindgen)
BINDGEN_HEADER := files/dm.h
BINDGEN_OPTIONS := --allowlist-var '^DM_.*$$' \
				--allowlist-var '^_IOC_.*$$' \
				--allowlist-type '^dm_ioctl$$' \
				--allowlist-type '^dm_target_spec$$' \
				--allowlist-type '^dm_name_list$$' \
				--wrap-unsafe-ops \
				--with-derive-default
BINDGEN_OUTPUT := ../src/dm/dm_control/defs.rs

dm:
ifeq ($(BINDGEN),)
	$(error "bindgen not found. Please install bindgen. \
	if you've been using cargo, run 'cargo install bindgen-cli'")
endif
	@echo "Generating [$(BINDGEN_OUTPUT)] with [$(BINDGEN_HEADER)] ..."
	@$(BINDGEN) $(BINDGEN_HEADER) $(BINDGEN_OPTIONS) -o $(BINDGEN_OUTPUT)
	@echo "Completed generating."

clean:
	@echo "Cleaning up..."
	@rm -f $(BINDGEN_OUTPUT)
	@echo "Cleaned up [$(BINDGEN_OUTPUT)]."
