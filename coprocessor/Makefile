.PHONY: all build clean fmt fmt-check tidy

LLVM_BIN := /opt/homebrew/opt/llvm@22/bin
SYSROOT := /Users/nilton/.platformio/packages/toolchain-xtensa-esp-elf/xtensa-esp-elf

ELF := .pio/build/esp32dev/firmware.elf
TIDY_STAMP := .pio/build/esp32dev/.tidy.stamp
FMT_STAMP := .pio/build/esp32dev/.fmt.stamp
FMT_CHECK_STAMP := .pio/build/esp32dev/.fmt_check.stamp

FMT_SRCS := $(wildcard src/*.cpp) $(wildcard src/*.h)
SRCS := $(FMT_SRCS) $(wildcard src/CMakeLists.txt) $(wildcard src/web/*) \
        $(wildcard include/*.h) platformio.ini CMakeLists.txt sdkconfig.defaults

all: build

build: $(ELF)

$(ELF): $(SRCS)
	pio run
	@touch $(ELF)

tidy: $(TIDY_STAMP)

$(TIDY_STAMP): $(SRCS)
	@PATH="$(LLVM_BIN):$$PATH" $(LLVM_BIN)/run-clang-tidy \
		-clang-tidy-binary=$(LLVM_BIN)/clang-tidy \
		-clang-apply-replacements-binary=$(LLVM_BIN)/clang-apply-replacements \
		-p . \
		-quiet \
		-hide-progress \
		-source-filter '.*/coprocessor/src/.*' \
		-header-filter '^(coprocessor/)?src/.*\.h$$' \
		-removed-arg='-mlongcalls' \
		-removed-arg='-fno-shrink-wrap' \
		-removed-arg='-fno-tree-switch-conversion' \
		-removed-arg='-fstrict-volatile-bitfields' \
		-extra-arg='--target=xtensa-esp-elf' \
		-extra-arg='-Qunused-arguments' \
		-extra-arg='-Wno-error' \
		-extra-arg='--sysroot=$(SYSROOT)' \
		-extra-arg='-isystem/Users/nilton/.platformio/packages/toolchain-xtensa-esp-elf/picolibc/include' \
		-extra-arg='-isystem/Users/nilton/.platformio/packages/toolchain-xtensa-esp-elf/xtensa-esp-elf/include/c++/15.2.0' \
		-extra-arg='-isystem/Users/nilton/.platformio/packages/toolchain-xtensa-esp-elf/xtensa-esp-elf/include/c++/15.2.0/xtensa-esp-elf/esp32' \
		-extra-arg='-isystem/Users/nilton/.platformio/packages/toolchain-xtensa-esp-elf/lib/gcc/xtensa-esp-elf/15.2.0/include' \
		-extra-arg='-isystem/Users/nilton/.platformio/packages/toolchain-xtensa-esp-elf/xtensa-esp-elf/include'
	@mkdir -p .pio/build/esp32dev && touch $(TIDY_STAMP)

fmt: $(FMT_STAMP)

$(FMT_STAMP): $(FMT_SRCS)
	$(LLVM_BIN)/clang-format -i $(FMT_SRCS)
	@mkdir -p .pio/build/esp32dev && touch $(FMT_STAMP) $(FMT_CHECK_STAMP)

fmt-check: $(FMT_CHECK_STAMP)

$(FMT_CHECK_STAMP): $(FMT_SRCS)
	$(LLVM_BIN)/clang-format --dry-run --Werror $(FMT_SRCS)
	@mkdir -p .pio/build/esp32dev && touch $(FMT_CHECK_STAMP)

clean:
	pio run -t clean
	rm -f $(TIDY_STAMP) $(FMT_STAMP) $(FMT_CHECK_STAMP)
