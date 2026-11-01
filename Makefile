#
#       Makefile
#       Multi-architecture build, ISO generation and verification targets
#
#       2026/9/30 By JiTianYu391
#       Copyright (C) 2026 ViudiraTech.
#

SHELL := /bin/sh

# A generated .config is authoritative; the checked-in defaults make a fresh
# clone buildable without visiting menuconfig first.
ifneq ($(wildcard .config),)
include .config
else
include .config-default
endif

ifeq ($(CONFIG_ARCH_AARCH64),y)
CONFIGURED_ARCH := aarch64
else ifeq ($(CONFIG_ARCH_RISCV64),y)
CONFIGURED_ARCH := riscv64
else
CONFIGURED_ARCH := x86_64
endif
ARCH ?= $(CONFIGURED_ARCH)
PROFILE ?= $(if $(filter y,$(CONFIG_PROFILE_RELEASE)),release,debug)
CPUS ?= $(CONFIG_QEMU_CPUS)
MEMORY_MIB ?= $(CONFIG_QEMU_MEMORY_MIB)

ifeq ($(ARCH),x86_64)
TARGET := x86_64-unknown-none
EFI := BOOTX64.EFI
QEMU := qemu-system-x86_64
FIRMWARE := /usr/share/OVMF/OVMF_CODE_4M.fd
MACHINE := q35
DTB_CMD := true
DTB_LINE :=
else ifeq ($(ARCH),aarch64)
TARGET := aarch64-unknown-none-softfloat
EFI := BOOTAA64.EFI
QEMU := qemu-system-aarch64
FIRMWARE := /usr/share/AAVMF/AAVMF_CODE.fd
MACHINE := virt
QEMU_CPU := -cpu max
DTB_CMD = $(QEMU) -machine $(MACHINE),dumpdtb=$(ISO_ROOT)/boot/limine/dtb.dtb $(QEMU_CPU) -smp $(CPUS) -m $(MEMORY_MIB)
DTB_LINE = dtb_path: boot():/boot/limine/dtb.dtb
else ifeq ($(ARCH),riscv64)
TARGET := riscv64imac-unknown-none-elf
EFI := BOOTRISCV64.EFI
QEMU := qemu-system-riscv64
FIRMWARE := /usr/share/qemu-efi-riscv64/RISCV_VIRT_CODE.fd
MACHINE := virt
DTB_CMD = $(QEMU) -machine $(MACHINE),dumpdtb=$(ISO_ROOT)/boot/limine/dtb.dtb $(QEMU_CPU) -smp $(CPUS) -m $(MEMORY_MIB)
DTB_LINE = dtb_path: boot():/boot/limine/dtb.dtb
else
$(error Unsupported ARCH=$(ARCH))
endif

ifeq ($(PROFILE),release)
CARGO_PROFILE := --release
else ifeq ($(PROFILE),debug)
CARGO_PROFILE :=
else
$(error Unsupported PROFILE=$(PROFILE))
endif

ifeq ($(CONFIG_BOOT_SELF_TEST),y)
CARGO_FEATURE_LIST := boot-self-test
else
CARGO_FEATURE_LIST :=
endif

ifeq ($(CONFIG_RISCV_SV48),y)
CARGO_FEATURE_LIST += riscv-sv48
endif
CARGO_FEATURES = $(foreach feature,$(CARGO_FEATURE_LIST),--features $(feature))

OUT := build/$(ARCH)/$(PROFILE)
KERNEL := target/$(TARGET)/$(PROFILE)/spacekernel
ISO := $(OUT)/SpaceKernel-$(ARCH).iso
ISO_ROOT := $(OUT)/iso
LIMINE_VERSION := v12.9.1
LIMINE_DIR := build/limine/$(LIMINE_VERSION)
LIMINE_ARCHIVE := build/downloads/limine-$(LIMINE_VERSION)-binary.tar.gz
LIMINE_SHA256 := 5cdebc518daa3af30b22c2322ba0dba2e0e2046fa8b087b9e13071b8dbcdcff4
LIMINE_URL ?= https://github.com/Limine-Bootloader/Limine/releases/download/$(LIMINE_VERSION)/limine-binary.tar.gz
DOWNLOAD_JOBS ?= 8
QEMU_FLAGS := -machine $(MACHINE) $(QEMU_CPU) -m $(MEMORY_MIB) -smp $(CPUS) -serial stdio -monitor none -no-reboot
QEMU_RESULT ?=
WITH_ROOTFS ?= 0
ROOTFS_SELFTEST ?= 0
ROOTFS_IMG := build/rootfs/fat32.img
ROOTFS_MODULE := $(if $(filter 1,$(WITH_ROOTFS)),module_path: boot():/rootfs.img,)

.PHONY: desktop desktop-iso desktop-spui desktop-rootfs desktop-run release-try all kernel iso rootfs-image busybox run debug test test-single test-host test-sched-model test-subsystem-model test-drm-abi check fmt menuconfig defconfig olddefconfig deps limine clean help
all: iso

defconfig:
	kconfig-conf --alldefconfig Kconfig

olddefconfig:
	kconfig-conf --olddefconfig Kconfig

menuconfig:
	kconfig-mconf Kconfig

deps:
	rustup component add rust-src
	rustup target add $(TARGET)
	cargo fetch --target $(TARGET)

kernel: deps
	@mkdir -p build
	@n=$$(cat build/kernel-build 2>/dev/null || echo 0); \
	n=$$((n + 1)); \
	echo $$n > build/kernel-build; \
	touch src/spk.rs; \
	SPK_BUILD_NUMBER="$$n" \
	SPK_BUILD_TIME="$$(date -u '+%Y-%m-%d %H:%M:%S UTC')" \
	SPK_BUILD_COMMIT="$$(git rev-parse --short HEAD 2>/dev/null || echo unknown)" \
	SPK_BUILD_USER="$$(id -un 2>/dev/null || echo unknown)" \
	SPK_BUILD_HOST="$$(hostname 2>/dev/null || echo unknown)" \
	SPK_BUILD_COMPILER="$$(rustc --version 2>/dev/null || echo rustc)" \
	cargo build --target $(TARGET) $(CARGO_PROFILE) $(CARGO_FEATURES)

$(LIMINE_ARCHIVE):
	mkdir -p build/downloads
	python3 tools/download_parallel.py '$(LIMINE_URL)' $@ $(LIMINE_SHA256) --jobs $(DOWNLOAD_JOBS)

$(LIMINE_DIR)/.extracted: $(LIMINE_ARCHIVE)
	printf '%s  %s\n' $(LIMINE_SHA256) $(LIMINE_ARCHIVE) | sha256sum -c -
	mkdir -p $(LIMINE_DIR)
	tar -xzf $< -C $(LIMINE_DIR) --strip-components=1
	touch $@

limine: $(LIMINE_DIR)/.extracted
	$(MAKE) -C $(LIMINE_DIR)

rootfs-image:
	WITH_ROOTFS=$(WITH_ROOTFS) ROOTFS_SELFTEST=$(ROOTFS_SELFTEST) python3 tools/build_rootfs.py

busybox:
	tools/build_busybox.sh

iso: kernel limine $(if $(filter 1,$(WITH_ROOTFS)),rootfs-image)
	rm -rf $(ISO_ROOT)
	mkdir -p $(ISO_ROOT)/EFI/BOOT $(ISO_ROOT)/boot/limine
	cp $(KERNEL) $(ISO_ROOT)/kernel.elf
	cp $(LIMINE_DIR)/$(EFI) $(ISO_ROOT)/EFI/BOOT/$(EFI)
	$(if $(filter 1,$(WITH_ROOTFS)),cp $(ROOTFS_IMG) $(ISO_ROOT)/rootfs.img,)
	cp $(LIMINE_DIR)/limine-bios.sys $(LIMINE_DIR)/limine-bios-cd.bin $(LIMINE_DIR)/limine-uefi-cd.bin $(ISO_ROOT)/boot/limine/
	$(DTB_CMD)
	python3 tools/render_limine.py boot/limine.conf.in $(ISO_ROOT)/limine.conf $(if $(filter y,$(CONFIG_KASLR)),yes,no) '$(DTB_LINE)' '$(ROOTFS_MODULE)'
	xorriso -as mkisofs -R -r -J $(if $(filter x86_64,$(ARCH)),-b boot/limine/limine-bios-cd.bin -no-emul-boot -boot-load-size 4 -boot-info-table,) -hfsplus -apm-block-size 2048 --efi-boot boot/limine/limine-uefi-cd.bin -efi-boot-part --efi-boot-image --protective-msdos-label -o $(ISO) $(ISO_ROOT)
	$(if $(filter x86_64,$(ARCH)),$(LIMINE_DIR)/limine bios-install $(ISO),true)
	@printf 'ISO ready: %s\n' '$(ISO)'

DESKTOP_CMDLINE ?= console=tty0 console=ttyS0 root=/dev/nvme0n1 init=/sbin/systemg
DESKTOP_IMAGE := build/rootfs/ext4.img

desktop-iso:
	$(MAKE) iso CMDLINE="$(DESKTOP_CMDLINE)" WITH_ROOTFS=0

desktop-spui:
	cd /tmp && RUSTUP_TOOLCHAIN=nightly-2026-05-24 cargo build --manifest-path "$(CURDIR)/user/desktop/Cargo.toml" --target x86_64-unknown-linux-musl --target-dir "$(CURDIR)/target" --release

desktop-rootfs: desktop-spui
	tools/build_userland.sh
	python3 tools/build_desktop_rootfs.py

desktop: desktop-iso desktop-rootfs

desktop-run: desktop
	qemu-system-x86_64 -enable-kvm -cpu host -machine q35 -m 4096 -smp $(CPUS) -serial stdio -monitor none -no-reboot -vga none -display gtk,zoom-to-fit=off -device virtio-gpu-pci,xres=1280,yres=800 -device virtio-tablet-pci -device virtio-keyboard-pci -drive file=$(DESKTOP_IMAGE),if=none,id=root,format=raw -device nvme,serial=SPROOT,drive=root -drive if=pflash,format=raw,readonly=on,file=$(FIRMWARE) -cdrom $(ISO)

# Reproducible release candidate requested by the release checklist.  The ISO
# boots the release kernel; the matching desktop rootfs is build/rootfs/ext4.img.
RELEASE_TRY := build/spacekernel-release-try.iso

release-try:
	$(MAKE) desktop PROFILE=release ARCH=x86_64
	cp build/x86_64/release/SpaceKernel-x86_64.iso $(RELEASE_TRY)
	cp build/rootfs/ext4.img build/spacekernel-release-try-rootfs.img
	@printf 'Release image ready: %s\n' '$(RELEASE_TRY)'
	@printf 'Release rootfs ready: build/spacekernel-release-try-rootfs.img\n'

DISK_DIR := build/disks
QEMU_STORAGE_FLAGS := -drive file=$(DISK_DIR)/nvme.img,if=none,id=nvme0,format=raw -device nvme,serial=SKNVME01,drive=nvme0 -device ich9-ahci,id=ahci0 -drive file=$(DISK_DIR)/sata.img,if=none,id=sata0,format=raw -device ide-hd,drive=sata0,bus=ahci0.0 $(if $(filter x86_64,$(ARCH)),-device piix3-ide,id=ide0 -drive file=$(DISK_DIR)/ide.img,if=none,id=pata0,format=raw -device ide-hd,drive=pata0,bus=ide0.0,)

disks:
	mkdir -p $(DISK_DIR)
	@for img in nvme.img sata.img ide.img; do \
	  [ -f $(DISK_DIR)/$$img ] || truncate -s 4M $(DISK_DIR)/$$img; \
	done

run: iso disks
	$(QEMU) $(QEMU_FLAGS) $(QEMU_STORAGE_FLAGS) -display gtk -drive if=pflash,format=raw,readonly=on,file=$(FIRMWARE) -cdrom $(ISO)

debug: iso disks
	$(QEMU) $(QEMU_FLAGS) $(QEMU_STORAGE_FLAGS) -display gtk -S -s -drive if=pflash,format=raw,readonly=on,file=$(FIRMWARE) -cdrom $(ISO)

test-sched-model:
	mkdir -p build
	rustc --edition=2024 --test tools/sched_model_test.rs -o build/sched-model-test
	build/sched-model-test

test-subsystem-model:
	mkdir -p build
	rustc --edition=2024 --test tools/subsystem_model_test.rs -o build/subsystem-model-test
	build/subsystem-model-test

test-drm-abi:
	mkdir -p build
	gcc tools/drm_abi_probe.c -o build/drm-abi-probe
	build/drm-abi-probe > build/drm-abi-probe.out
	@grep -F -f build/drm-abi-probe.out src/gpu/abi.rs > /dev/null || (echo "DRM ABI mismatch" && exit 1)

test-host: test-sched-model test-subsystem-model test-drm-abi

test-single: iso
	python3 tools/check_no_simd.py $(ARCH) $(KERNEL)
	python3 tools/qemu_test.py $(ARCH) $(ISO) $(FIRMWARE) $(CPUS) $(MEMORY_MIB) --machine '$(MACHINE)' $(if $(strip $(QEMU_CPU)),--cpu $(word 2,$(QEMU_CPU)),) $(if $(filter y,$(CONFIG_BOOT_SELF_TEST)),--require 'SELFTEST_OK' --require 'BOOT_OK',) $(if $(filter x86_64,$(ARCH)),--gpu venus,) --online-cpus $(CPUS) $(if $(QEMU_RESULT),--result '$(QEMU_RESULT)',)

ifeq ($(origin ARCH),command line)
test: test-single
else
test:
	@python3 tools/test_matrix.py
endif

check:
	cargo fmt --all -- --check
	@for target in x86_64-unknown-none aarch64-unknown-none-softfloat riscv64imac-unknown-none-elf; do \
	  rustup target add $$target && cargo check --target $$target --all-features || exit 1; \
	done

fmt:
	cargo fmt --all

clean:
	cargo clean
	rm -rf build

help:
	@printf '%s\n' \
	  'make menuconfig   Configure architecture, profile, KASLR and QEMU' \
	  'make defconfig    Restore checked-in Kconfig defaults' \
	  'make             Build bootable ISO' \
	  'make run         Boot ISO in QEMU' \
	  'make debug       Start QEMU paused with GDB port 1234' \
	  'make test        Host model checks + x86_64/aarch64/riscv64 boot matrix' \
	  'make test ARCH=  Single-architecture boot smoke test' \
	  'make check       Format and check all target architectures' \
	  'make release-try Build release desktop ISO at build/spacekernel-release-try.iso'
