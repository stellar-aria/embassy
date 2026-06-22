# Examples for Cortex-A9 on QEMU `xilinx-zynq-a9`

These examples demonstrate the generic `platform-cortex-ar` executor running on a
Cortex-A9 MPCore, including the **interrupt-mode executor** driven by a GIC
Software-Generated Interrupt (SGI) on the Cortex-A9's built-in GICv1.

## Running

You need `qemu-system-arm` (version 9 or higher) on your `PATH`. Then:

```sh
cargo run --bin interrupt_executor
```

The example prints over semihosting and exits QEMU with code 0 on success
(look for the `PASS` line).

## Notes

- The GIC base is found from `PERIPHBASE` via the Cortex-A CBAR register
  (`MRC p15, 4, Rt, c15, c0, 0`); the GIC Distributor is at `PERIPHBASE + 0x1000`.
- `aarch32-rt`'s armv7-a startup does not program `VBAR`, so the example sets it
  to the linked vector table before enabling interrupts.
- `embassy_executor::InterruptExecutor` only *triggers* the SGI when a task is
  woken. Configuring/enabling the SGI and writing the IRQ handler (acknowledge
  via `GICC_IAR`, then `on_interrupt()`, then end-of-interrupt via `GICC_EOIR`)
  is the application's responsibility — see `src/bin/interrupt_executor.rs`.
