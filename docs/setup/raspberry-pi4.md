# Raspberry Pi 4 setup and execution

Use a Raspberry Pi 4 Model B with BCM2711 Cortex-A72 CPUs, a firmware armstub
implementing the [secondary contract](../architecture/platform.md#secondary-firmware-contract),
and U-Boot entering Non-secure AArch64 EL2. A PSCI startup description or another
firmware regime is not supported by the current physical startup backend.

## Console and host services

The supported physical console is the AUX mini UART, normally GPIO14 TX
(header pin 8), GPIO15 RX (pin 10), with common ground and a 3.3 V UART adapter.
Use the board's working 115200 8N1 firmware/U-Boot setup. `enable_uart=1` in
firmware configuration enables the interface and stabilizes the mini-UART clock.
Confirm `/chosen/stdout-path`, aliases and compatible strings; overlays routing
a PL011 to those pins need a different physical-console backend.

Provide a working TFTP server reachable from U-Boot and a serial console. For
automated testing, use the existing rpictl configuration/daemon and keep UART
access exclusive to the runner. The [hardware guide](../testing/hardware-tests.md)
describes the validated lab network and power-cycle procedure.

## Validate the live layout

Boot/reset into a fresh U-Boot session and interrupt autoboot. Re-entering
tvisor repeatedly from an old session is unsupported. Inspect the same boot's
layout before downloads:

```text
bdinfo
printenv fdt_addr fdt_addr_r fdtcontroladdr kernel_addr_r loadaddr serverip
fdt addr ${fdt_addr}
fdt print /chosen stdout-path
fdt print /aliases
fdt print /soc ranges
fdt print /cpus
```

Use the working DTB supplied by `fdt_addr`; U-Boot's internal control DTB can
have a different address. Review RAM banks, firmware reservations, relocated
U-Boot, stack, tables, live DTB and all download buffers. LMB/bdinfo are deployment
evidence, not tvisor runtime arguments or authoritative post-takeover ownership.

For each new ELF, obtain `main`, `__image_start` and `__image_end` using the
[build commands](build.md#elf-placement). Both the complete runtime footprint
and downloaded ELF file must avoid live U-Boot/firmware state. The staged Linux
file must avoid those regions, tvisor and the DTB. `filesize` changes after
each TFTP download; preserve the Linux file length before invoking tvisor.

The validated lab profile uses the following addresses. These are conditional
deployment values, not an allocation policy or portable board ABI:

| Object | Lab address/limit |
|---|---|
| tvisor ELF staging and link base | `0x04000000` |
| tvisor file/runtime upper bound | Below page-rounded DTB start `0x2eff1000` |
| Working host DTB address | `0x2eff1f00` |
| Linux Image staging | `0x30100000` |
| Linux source upper bound | Below observed live U-Boot arena `0x36b2b000` |

The observed U-Boot arena extends to `0x38000000`, with relocated executable at
`0x37f4d000`. The observed RAM banks end at `0x38000000` and span
`[0x40000000, 0xfc000000)` above the firmware hole. Firmware, RAM size, DTB,
U-Boot build and boot sequence can change these observations. Stop and review
the layout if they differ; do not merely relax runner checks.

## Publish the images

Build the ELF and [Linux guest](build.md#linux-guest), or choose the verified
guest fixture. For a TFTP root at `/srv/tftp`, an example manual publication is:

```sh
sudo install -m 0644 target/aarch64-unknown-none/debug/tvisor /srv/tftp/tvisor
sudo install -m 0644 guest/build/arm64/arch/arm64/boot/Image /srv/tftp/Image
```

Automated runs use unique manifest filenames and fresh result directories,
preserving previous artifacts. Publish exactly the files named by that manifest.

## Launch from U-Boot

For the validated layout, replace `<tftp-server-ip>` with the configured server:

```text
setenv autostart no
tftpboot 0x04000000 <tftp-server-ip>:tvisor
tftpboot 0x30100000 <tftp-server-ip>:Image
setenv guest_size ${filesize}
go <main-address-from-this-ELF> fdt=${fdt_addr} image=0x30100000,${guest_size}
```

The direct `go` workflow enters the file-backed ELF text at the symbol address;
the entry explicitly clears BSS and table setup zeros its NOLOAD arena. The
complete file and runtime ranges must both have been validated. The automated
runner instead uses U-Boot's ELF loader with the same tagged handoff:

```text
bootelf 0x04000000 fdt=${fdt_addr} image=0x30100000,${guest_size}
```

Use an ELF-loading command only with the staging/segment layout verified for
that U-Boot build. No `lmb=`, `bootmem=` or returnable diagnostic mode is needed.
Do not save environment changes or write the SD card during regression tests.

## Expected result and recovery

An ordinary image prints `tvisor: entering one-vCPU Linux guest`, Linux startup
and `BusyBox initramfs ready`. Console input should reach the guest shell.
Diagnostics builds additionally report physical CPU resources, startup
checkpoints, management checks and responsive parked workers. Only explicitly
selected acceptance builds emit scratch remap/stress/fault-test results.

Failure after console initialization reports a reason and halts; failure before
console discovery can be silent. There is no return to the U-Boot prompt. Use
a fresh reset/power cycle for the next run. Automated cleanup verifies final
power-off; if the board has unexpectedly booted its SD-card OS, use the runner's
graceful shutdown/ownership checks rather than forcing an unknown power cut.

For fault investigation, retain the complete ELF, symbols and fresh UART output.
[Host testing guidance](../testing/host-tests.md#investigating-an-abort) describes
how to correlate syndrome, exception PC and linked instructions.
