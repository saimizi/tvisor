# Build a minimal, single-core arm64 Linux guest image.
#
# Prerequisites: curl, tar, make, and LLVM/Clang with LLD for the kernel. The
# BusyBox initramfs additionally needs an arm64 GNU cross-toolchain and libc
# sysroot (for example, `aarch64-linux-gnu-gcc`).

KERNEL_VERSION := 6.18.2
KERNEL_ARCHIVE := linux-$(KERNEL_VERSION).tar.xz
KERNEL_URL := https://cdn.kernel.org/pub/linux/kernel/v6.x/$(KERNEL_ARCHIVE)
KERNEL_DIR := linux-$(KERNEL_VERSION)
BUILD_DIR := build/arm64
BUSYBOX_VERSION := 1.38.0
BUSYBOX_ARCHIVE := busybox-$(BUSYBOX_VERSION).tar.bz2
BUSYBOX_URL := https://busybox.net/downloads/$(BUSYBOX_ARCHIVE)
BUSYBOX_DIR := busybox-$(BUSYBOX_VERSION)
BUSYBOX_BUILD_DIR := build/busybox-arm64
INITRAMFS_ROOT := build/initramfs-root
INITRAMFS_CPIO := build/initramfs.cpio
INITRAMFS := $(INITRAMFS_CPIO).gz

ARCH ?= arm64
CROSS_COMPILE ?= aarch64-linux-gnu-
LLVM ?= 1
JOBS ?= $(shell nproc)

.PHONY: all download extract config image busybox initramfs clean distclean

all: image initramfs

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
# from the guest DTB, and INITRAMFS_SOURCE embeds the generated cpio archive.
$(BUILD_DIR)/.config: Makefile $(KERNEL_DIR)/Makefile $(INITRAMFS_CPIO)
	$(MAKE) -C $(KERNEL_DIR) O=$(abspath $(BUILD_DIR)) ARCH=$(ARCH) CROSS_COMPILE=$(CROSS_COMPILE) LLVM=$(LLVM) tinyconfig
	$(KERNEL_DIR)/scripts/config --file $(abspath $@) --enable 64BIT
	$(KERNEL_DIR)/scripts/config --file $(abspath $@) --enable ARM64
	$(KERNEL_DIR)/scripts/config --file $(abspath $@) --enable MMU
	$(KERNEL_DIR)/scripts/config --file $(abspath $@) --enable SMP
	$(KERNEL_DIR)/scripts/config --file $(abspath $@) --enable OF
	$(KERNEL_DIR)/scripts/config --file $(abspath $@) --enable OF_EARLY_FLATTREE
	$(KERNEL_DIR)/scripts/config --file $(abspath $@) --enable TTY
	# devtmpfs provides /dev/console before /init runs, so the initramfs shell
	# retains its standard streams after the normal PL011 console is registered.
	$(KERNEL_DIR)/scripts/config --file $(abspath $@) --enable DEVTMPFS
	$(KERNEL_DIR)/scripts/config --file $(abspath $@) --enable DEVTMPFS_MOUNT
	$(KERNEL_DIR)/scripts/config --file $(abspath $@) --enable SERIAL_AMBA_PL011
	$(KERNEL_DIR)/scripts/config --file $(abspath $@) --enable SERIAL_AMBA_PL011_CONSOLE
	$(KERNEL_DIR)/scripts/config --file $(abspath $@) --enable SERIAL_EARLYCON
	$(KERNEL_DIR)/scripts/config --file $(abspath $@) --enable IRQCHIP
	$(KERNEL_DIR)/scripts/config --file $(abspath $@) --enable ARM_GIC
	$(KERNEL_DIR)/scripts/config --file $(abspath $@) --enable ARM_ARCH_TIMER
	$(KERNEL_DIR)/scripts/config --file $(abspath $@) --enable PRINTK
	# Linux 6.18.2 has removed CONFIG_EARLY_PRINTK; SERIAL_EARLYCON above is its replacement.
	# BLK_DEV_INITRD gates CONFIG_INITRAMFS_SOURCE in Linux Kconfig.
	$(KERNEL_DIR)/scripts/config --file $(abspath $@) --enable BLK_DEV_INITRD
	# The initramfs starts with a #!/bin/sh script whose interpreter is the
	# statically linked BusyBox binary, so tinyconfig also needs both binary
	# format handlers to execute the initial userspace.
	$(KERNEL_DIR)/scripts/config --file $(abspath $@) --enable BINFMT_ELF
	$(KERNEL_DIR)/scripts/config --file $(abspath $@) --enable BINFMT_SCRIPT
	$(KERNEL_DIR)/scripts/config --file $(abspath $@) --set-str INITRAMFS_SOURCE "$(abspath $(INITRAMFS_CPIO))"
	$(MAKE) -C $(KERNEL_DIR) O=$(abspath $(BUILD_DIR)) ARCH=$(ARCH) CROSS_COMPILE=$(CROSS_COMPILE) LLVM=$(LLVM) olddefconfig

image: $(INITRAMFS_CPIO) $(BUILD_DIR)/arch/arm64/boot/Image

$(BUILD_DIR)/arch/arm64/boot/Image: $(BUILD_DIR)/.config
	$(MAKE) -C $(KERNEL_DIR) O=$(abspath $(BUILD_DIR)) ARCH=$(ARCH) CROSS_COMPILE=$(CROSS_COMPILE) LLVM=$(LLVM) -j$(JOBS) Image

busybox: $(BUSYBOX_BUILD_DIR)/busybox

$(BUSYBOX_ARCHIVE):
	curl --fail --location --remote-name $(BUSYBOX_URL)

$(BUSYBOX_DIR)/Makefile: $(BUSYBOX_ARCHIVE)
	tar -xf $<
	touch $@

# BusyBox is deliberately statically linked so the initramfs needs no shared
# libraries. A GNU arm64 cross-toolchain, including its libc sysroot, is
# required for this userspace build.
$(BUSYBOX_BUILD_DIR)/.config: Makefile $(BUSYBOX_DIR)/Makefile
	mkdir -p $(BUSYBOX_BUILD_DIR)
	cp busybox.config $(abspath $@)

$(BUSYBOX_BUILD_DIR)/busybox: $(BUSYBOX_BUILD_DIR)/.config
	$(MAKE) -C $(BUSYBOX_DIR) O=$(abspath $(BUSYBOX_BUILD_DIR)) ARCH=$(ARCH) CROSS_COMPILE=$(CROSS_COMPILE) -j$(JOBS)

initramfs: $(INITRAMFS)

$(INITRAMFS_CPIO): $(BUSYBOX_BUILD_DIR)/busybox initramfs/init
	rm -rf $(INITRAMFS_ROOT)
	install -D -m 0755 $(BUSYBOX_BUILD_DIR)/busybox $(INITRAMFS_ROOT)/bin/busybox
	ln -s busybox $(INITRAMFS_ROOT)/bin/sh
	install -D -m 0755 initramfs/init $(INITRAMFS_ROOT)/init
	mkdir -p $(INITRAMFS_ROOT)/dev $(INITRAMFS_ROOT)/proc $(INITRAMFS_ROOT)/sys
	# fakeroot records the console's c 5:1 metadata in the CPIO archive without
	# requiring host privileges to create a device node in the build tree.
	fakeroot -- sh -c 'mknod -m 0600 "$(abspath $(INITRAMFS_ROOT))/dev/console" c 5 1 && cd "$(abspath $(INITRAMFS_ROOT))" && find . -print0 | sort -z | cpio --null -o -H newc > "$(abspath $@)"'

$(INITRAMFS): $(INITRAMFS_CPIO)
	gzip -9 -c $< > $@

clean:
	$(MAKE) -C $(KERNEL_DIR) O=$(abspath $(BUILD_DIR)) ARCH=$(ARCH) CROSS_COMPILE=$(CROSS_COMPILE) LLVM=$(LLVM) clean
	$(MAKE) -C $(BUSYBOX_DIR) O=$(abspath $(BUSYBOX_BUILD_DIR)) ARCH=$(ARCH) CROSS_COMPILE=$(CROSS_COMPILE) clean

distclean:
	rm -rf $(BUILD_DIR) $(BUSYBOX_BUILD_DIR) $(INITRAMFS_ROOT) $(INITRAMFS_CPIO) $(INITRAMFS) $(KERNEL_DIR) $(BUSYBOX_DIR) $(KERNEL_ARCHIVE) $(BUSYBOX_ARCHIVE)
