# Build a minimal, single-core arm64 Linux guest image.
#
# Prerequisites: curl, tar, make, and LLVM/Clang with LLD.  A GNU cross-toolchain
# can be selected instead with `make LLVM= CROSS_COMPILE=aarch64-linux-gnu-`.

KERNEL_VERSION := 6.18.2
KERNEL_ARCHIVE := linux-$(KERNEL_VERSION).tar.xz
KERNEL_URL := https://cdn.kernel.org/pub/linux/kernel/v6.x/$(KERNEL_ARCHIVE)
KERNEL_DIR := linux-$(KERNEL_VERSION)
BUILD_DIR := build/arm64

ARCH ?= arm64
CROSS_COMPILE ?= aarch64-linux-gnu-
LLVM ?= 1
JOBS ?= $(shell nproc)

.PHONY: all download extract config image clean distclean

all: image

download: $(KERNEL_ARCHIVE)

$(KERNEL_ARCHIVE):
	curl --fail --location --remote-name $(KERNEL_URL)

extract: $(KERNEL_DIR)/Makefile

$(KERNEL_DIR)/Makefile: $(KERNEL_ARCHIVE)
	tar -xf $<
	# tar preserves the timestamp stored in the archive; refresh the target so
	# make does not extract again just because the downloaded archive is newer.
	touch $@

config: $(BUILD_DIR)/.config

# `tinyconfig` gives the smallest upstream configuration. CPU topology comes
# from the guest DTB. INITRAMFS_SOURCE is kept empty as a second safeguard
# against embedding a cpio archive.
$(BUILD_DIR)/.config: Makefile $(KERNEL_DIR)/Makefile
	$(MAKE) -C $(KERNEL_DIR) O=$(abspath $(BUILD_DIR)) ARCH=$(ARCH) CROSS_COMPILE=$(CROSS_COMPILE) LLVM=$(LLVM) tinyconfig
	$(KERNEL_DIR)/scripts/config --file $(abspath $@) --enable 64BIT
	$(KERNEL_DIR)/scripts/config --file $(abspath $@) --enable ARM64
	$(KERNEL_DIR)/scripts/config --file $(abspath $@) --enable MMU
	$(KERNEL_DIR)/scripts/config --file $(abspath $@) --enable SMP
	$(KERNEL_DIR)/scripts/config --file $(abspath $@) --enable OF
	$(KERNEL_DIR)/scripts/config --file $(abspath $@) --enable OF_EARLY_FLATTREE
	$(KERNEL_DIR)/scripts/config --file $(abspath $@) --enable TTY
	$(KERNEL_DIR)/scripts/config --file $(abspath $@) --enable SERIAL_AMBA_PL011
	$(KERNEL_DIR)/scripts/config --file $(abspath $@) --enable SERIAL_AMBA_PL011_CONSOLE
	$(KERNEL_DIR)/scripts/config --file $(abspath $@) --enable SERIAL_EARLYCON
	$(KERNEL_DIR)/scripts/config --file $(abspath $@) --enable IRQCHIP
	$(KERNEL_DIR)/scripts/config --file $(abspath $@) --enable ARM_GIC
	$(KERNEL_DIR)/scripts/config --file $(abspath $@) --enable ARM_ARCH_TIMER
	$(KERNEL_DIR)/scripts/config --file $(abspath $@) --enable PRINTK
	# Linux 6.18.2 has removed CONFIG_EARLY_PRINTK; SERIAL_EARLYCON above is its replacement.
	$(KERNEL_DIR)/scripts/config --file $(abspath $@) --disable BLK_DEV_INITRD
	$(KERNEL_DIR)/scripts/config --file $(abspath $@) --set-str INITRAMFS_SOURCE ""
	$(MAKE) -C $(KERNEL_DIR) O=$(abspath $(BUILD_DIR)) ARCH=$(ARCH) CROSS_COMPILE=$(CROSS_COMPILE) LLVM=$(LLVM) olddefconfig

image: $(BUILD_DIR)/arch/arm64/boot/Image

$(BUILD_DIR)/arch/arm64/boot/Image: $(BUILD_DIR)/.config
	$(MAKE) -C $(KERNEL_DIR) O=$(abspath $(BUILD_DIR)) ARCH=$(ARCH) CROSS_COMPILE=$(CROSS_COMPILE) LLVM=$(LLVM) -j$(JOBS) Image

clean:
	$(MAKE) -C $(KERNEL_DIR) O=$(abspath $(BUILD_DIR)) ARCH=$(ARCH) CROSS_COMPILE=$(CROSS_COMPILE) LLVM=$(LLVM) clean

distclean:
	rm -rf $(BUILD_DIR) $(KERNEL_DIR) 
