.PHONY: all fmt fmt-check clean

rwildcard = $(foreach d,$(wildcard $(1:=/*)),$(call rwildcard,$d,$2) $(filter $(subst *,%,$2),$d))

STAMP := target/.fmt.stamp
CHECK_STAMP := target/.fmt_check.stamp
SRCS := $(call rwildcard,apps crates,*.rs) Cargo.toml $(wildcard rustfmt.toml ../rustfmt.toml)

all: fmt

fmt: $(STAMP)

$(STAMP): $(SRCS)
	cargo +nightly fmt --all
	@mkdir -p target && touch $(STAMP) $(CHECK_STAMP)

fmt-check: $(CHECK_STAMP)

$(CHECK_STAMP): $(SRCS)
	cargo +nightly fmt --all -- --check
	@mkdir -p target && touch $(CHECK_STAMP)

clean:
	rm -f $(STAMP) $(CHECK_STAMP)
