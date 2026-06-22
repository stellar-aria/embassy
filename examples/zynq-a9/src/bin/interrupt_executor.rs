//! Interrupt-mode executor on a Cortex-A9 (QEMU `xilinx-zynq-a9`), driven by a
//! GIC Software-Generated Interrupt (SGI).
//!
//! `embassy_executor::InterruptExecutor` on `platform-cortex-ar` pends an SGI on
//! the current CPU whenever a task is woken. The user is responsible for setting
//! up the GIC and writing the interrupt handler; the executor only triggers the
//! SGI. This example:
//!
//! 1. Sets `VBAR` (aarch32-rt's armv7-a startup does not), then enables the GIC.
//! 2. Starts an interrupt executor on SGI 0 and spawns a task on it.
//! 3. From thread mode, signals the task a few times. Each signal wakes the
//!    task, which pends SGI 0, which fires the IRQ, which polls the task.
//!
//! Run with `cargo run --bin interrupt_executor` (needs `qemu-system-arm`).

#![no_std]
#![no_main]

use core::ptr::{read_volatile, write_volatile};

use embassy_executor::{InterruptExecutor, SendSpawner};
use embassy_sync::blocking_mutex::raw::CriticalSectionRawMutex;
use embassy_sync::signal::Signal;
use semihosting::println;

/// SGI used to drive the interrupt executor (valid range 0..=15).
const SGI: u8 = 0;

static EXEC: InterruptExecutor = InterruptExecutor::new();
static SIGNAL: Signal<CriticalSectionRawMutex, u32> = Signal::new();

#[panic_handler]
fn panic(info: &core::panic::PanicInfo) -> ! {
    println!("PANIC: {}", info);
    semihosting::process::exit(1);
}

/// Minimal GICv1/v2 register access for the Cortex-A9's built-in GIC.
mod gic {
    use core::ptr::write_volatile;

    /// Read `PERIPHBASE` from the Cortex-A CBAR (`MRC p15, 4, Rt, c15, c0, 0`).
    pub fn periphbase() -> *mut u32 {
        let base: u32;
        // Safety: reading PERIPHBASE via CBAR has no side effects.
        unsafe {
            core::arch::asm!("mrc p15, 4, {}, c15, c0, 0", out(reg) base, options(nomem, nostack));
        }
        (base & 0xFFF0_0000) as *mut u32
    }

    /// GIC Distributor base (`PERIPHBASE + 0x1000`).
    pub fn gicd() -> *mut u32 {
        periphbase().wrapping_byte_add(0x1000).cast()
    }

    /// GIC CPU Interface base (`PERIPHBASE + 0x100`).
    pub fn gicc() -> *mut u32 {
        periphbase().wrapping_byte_add(0x100).cast()
    }

    /// Enable the distributor + CPU interface and unmask the given SGI.
    ///
    /// # Safety
    ///
    /// Must be called once, before interrupts are enabled.
    pub unsafe fn init(sgi: u8) {
        unsafe {
            write_volatile(gicd().add(0x000 / 4), 1); // GICD_CTLR: enable distributor
            write_volatile(gicd().add(0x100 / 4), 1 << sgi); // GICD_ISENABLER0: enable SGI
            write_volatile(gicc().add(0x004 / 4), 0xFF); // GICC_PMR: accept all priorities
            write_volatile(gicc().add(0x000 / 4), 1); // GICC_CTLR: enable CPU interface
        }
    }
}

#[aarch32_rt::irq]
fn irq() {
    unsafe {
        let iar = read_volatile(gic::gicc().add(0x00C / 4)); // GICC_IAR: acknowledge
        let intid = iar & 0x3FF;
        if intid == SGI as u32 {
            EXEC.on_interrupt();
        }
        write_volatile(gic::gicc().add(0x010 / 4), iar); // GICC_EOIR: end of interrupt
    }
}

#[embassy_executor::task]
async fn worker() {
    println!("[irq-exec] task started on SGI {}", SGI);
    loop {
        let n = SIGNAL.wait().await;
        println!("[irq-exec] woke via SGI, got {}", n);
        if n == 3 {
            println!("PASS");
            semihosting::process::exit(0);
        }
    }
}

#[aarch32_rt::entry]
fn kmain() -> ! {
    println!("[boot] cortex-a9 GIC interrupt-executor example");

    unsafe {
        // aarch32-rt's armv7-a startup does not set VBAR, so point it at the
        // linked vector table before enabling interrupts.
        extern "C" {
            static _vector_table: u32;
        }
        aarch32_cpu::register::Vbar::write(aarch32_cpu::register::Vbar(
            &_vector_table as *const u32 as u32,
        ));

        gic::init(SGI);
        aarch32_cpu::interrupt::enable();
    }

    // Start the interrupt-mode executor on SGI 0 and spawn a task on it.
    // Spawning enqueues the task, which pends SGI 0 -> IRQ -> on_interrupt -> poll.
    let spawner: SendSpawner = EXEC.start(SGI);
    let Ok(token) = worker() else {
        panic!("spawn failed");
    };
    spawner.spawn(token);

    // From thread mode, repeatedly wake the task. Each wake pends SGI 0 again,
    // exercising the full wake -> __pender(SGIR) -> IRQ -> poll path.
    for n in 1..=3u32 {
        for _ in 0..200_000 {
            core::hint::spin_loop();
        }
        SIGNAL.signal(n);
    }

    loop {
        aarch32_cpu::asm::wfi();
    }
}
