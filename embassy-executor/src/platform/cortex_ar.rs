#[cfg(arm_profile = "legacy")]
compile_error!("`arch-cortex-ar` does not support the legacy ARM profile, WFE/SEV are not available.");

/// Platform-supplied GIC Distributor base, or 0 to derive it from CBAR.
///
/// `0` (the default) means "discover via CBAR/PERIPHBASE", which is correct for
/// Cortex-A MPCore parts where the GIC sits at `PERIPHBASE + 0x1000`. On SoCs
/// whose GIC is integrated at a fixed address unrelated to CBAR — e.g. the
/// single-core Renesas RZ/A1 (GIC Distributor at `0xE820_1000`, while CBAR reads
/// `0xF000_0000`) — the application must call [`set_gicd_base`] with the real
/// Distributor address before starting an interrupt executor.
#[cfg(feature = "executor-interrupt")]
static GICD_BASE_OVERRIDE: core::sync::atomic::AtomicUsize = core::sync::atomic::AtomicUsize::new(0);

/// Override the GIC Distributor base address used by the interrupt-executor SGI
/// pender, bypassing CBAR/PERIPHBASE discovery.
///
/// Pass the Distributor base (the MMIO block whose `GICD_SGIR` lives at offset
/// `0xF00`). Required on parts where the GIC is not at `PERIPHBASE + 0x1000`
/// (e.g. Renesas RZ/A1: `0xE820_1000`). Must be called once at startup, before
/// any interrupt executor is started or any task on it is woken.
#[cfg(feature = "executor-interrupt")]
pub fn set_gicd_base(base: usize) {
    GICD_BASE_OVERRIDE.store(base, core::sync::atomic::Ordering::Relaxed);
}

/// GIC Distributor base for the Cortex-A built-in GIC.
///
/// Uses the address set by [`set_gicd_base`] when one was provided; otherwise
/// derives it from the CP15 Configuration Base Address Register (CBAR /
/// PERIPHBASE). On Cortex-A MPCore parts (A5/A7/A9/A15) `PERIPHBASE` is read via
/// `MRC p15, 4, Rt, c15, c0, 0`, and the GIC Distributor sits at
/// `PERIPHBASE + 0x1000`. (Note this differs from the Cortex-R CBAR encoding
/// used by `aarch32_cpu::register::ImpCbar`; interrupt-executor support targets
/// the Cortex-A GICv1/v2 MMIO interface.)
#[cfg(feature = "executor-interrupt")]
fn gicd_base() -> *mut u32 {
    let override_base = GICD_BASE_OVERRIDE.load(core::sync::atomic::Ordering::Relaxed);
    if override_base != 0 {
        return override_base as *mut u32;
    }
    let periphbase: u32;
    // Safety: reading PERIPHBASE via CBAR has no side effects.
    unsafe {
        core::arch::asm!("mrc p15, 4, {}, c15, c0, 0", out(reg) periphbase, options(nomem, nostack));
    }
    ((periphbase & 0xFFF0_0000) + 0x1000) as *mut u32
}

#[unsafe(export_name = "__pender")]
#[cfg(any(feature = "executor-thread", feature = "executor-interrupt"))]
fn __pender(context: *mut ()) {
    // `context` is `usize::MAX` for the thread executor (created by `Executor::run`),
    // or the SGI interrupt id for an interrupt executor (created by `start`).
    let context = context as usize;

    #[cfg(feature = "executor-thread")]
    // Try to make Rust optimize the branching away if we only use thread mode.
    if !cfg!(feature = "executor-interrupt") || context == THREAD_PENDER {
        aarch32_cpu::asm::sev();
        return;
    }

    #[cfg(feature = "executor-interrupt")]
    {
        let sgi_id = (context & 0xF) as u32;
        // GICD_SGIR is at offset 0xF00 from the Distributor base.
        // TargetListFilter = 0b10 routes the SGI to the requesting CPU only.
        const SGIR_TO_SELF: u32 = 0b10 << 24;
        // Safety: writing GICD_SGIR only pends the configured SGI; it has no other effect.
        unsafe {
            gicd_base().add(0xF00 / 4).write_volatile(SGIR_TO_SELF | sgi_id);
        }
    }
}

#[cfg(feature = "executor-thread")]
pub use thread::*;
#[cfg(feature = "executor-thread")]
mod thread {
    pub(super) const THREAD_PENDER: usize = usize::MAX;

    use core::marker::PhantomData;

    use aarch32_cpu::asm::wfe;
    pub use embassy_executor_macros::main_cortex_ar as main;

    use crate::{Spawner, raw};

    /// Thread mode executor, using WFE/SEV.
    ///
    /// This is the simplest and most common kind of executor. It runs on
    /// thread mode (at the lowest priority level), and uses the `WFE` ARM instruction
    /// to sleep when it has no more work to do. When a task is woken, a `SEV` instruction
    /// is executed, to make the `WFE` exit from sleep and poll the task.
    ///
    /// This executor allows for ultra low power consumption for chips where `WFE`
    /// triggers low-power sleep without extra steps. If your chip requires extra steps,
    /// you may use [`raw::Executor`] directly to program custom behavior.
    pub struct Executor {
        inner: raw::Executor,
        not_send: PhantomData<*mut ()>,
    }

    impl Executor {
        /// Create a new Executor.
        pub fn new() -> Self {
            Self {
                inner: raw::Executor::new(THREAD_PENDER as *mut ()),
                not_send: PhantomData,
            }
        }

        /// Run the executor.
        ///
        /// The `init` closure is called with a [`Spawner`] that spawns tasks on
        /// this executor. Use it to spawn the initial task(s). After `init` returns,
        /// the executor starts running the tasks.
        ///
        /// To spawn more tasks later, you may keep copies of the [`Spawner`] (it is `Copy`),
        /// for example by passing it as an argument to the initial tasks.
        ///
        /// This function requires `&'static mut self`. This means you have to store the
        /// Executor instance in a place where it'll live forever and grants you mutable
        /// access. There's a few ways to do this:
        ///
        /// - a [StaticCell](https://docs.rs/static_cell/latest/static_cell/) (safe)
        /// - a `static mut` (unsafe)
        /// - a local variable in a function you know never returns (like `fn main() -> !`), upgrading its lifetime with `transmute`. (unsafe)
        ///
        /// This function never returns.
        pub fn run(&'static mut self, init: impl FnOnce(Spawner)) -> ! {
            init(self.inner.spawner());

            loop {
                unsafe {
                    self.inner.poll();
                }
                wfe();
            }
        }
    }
}

#[cfg(feature = "executor-interrupt")]
pub use interrupt::*;
#[cfg(feature = "executor-interrupt")]
mod interrupt {
    use core::cell::{Cell, UnsafeCell};
    use core::mem::MaybeUninit;

    use critical_section::Mutex;

    use crate::raw;

    /// Interrupt executor.
    ///
    /// This executor runs tasks in interrupt mode. The interrupt handler is set up
    /// to poll tasks, and when a task is woken the interrupt is pended from software
    /// via a GIC Software-Generated Interrupt (SGI).
    ///
    /// This allows running async tasks at a priority higher than thread mode. One
    /// use case is to leave thread mode free for non-async tasks. Another use case is
    /// to run multiple executors: one in thread mode for low priority tasks and another in
    /// interrupt mode for higher priority tasks. Higher priority tasks will preempt lower
    /// priority ones.
    ///
    /// It is even possible to run multiple interrupt mode executors at different priorities,
    /// by assigning different priorities to the SGIs.
    ///
    /// It is somewhat more complex to use, it's recommended to use the thread-mode
    /// [`Executor`](crate::Executor) instead, if it works for your use case.
    pub struct InterruptExecutor {
        started: Mutex<Cell<bool>>,
        executor: UnsafeCell<MaybeUninit<crate::raw::Executor>>,
    }

    unsafe impl Send for InterruptExecutor {}
    unsafe impl Sync for InterruptExecutor {}

    impl InterruptExecutor {
        /// Create a new, not started `InterruptExecutor`.
        #[inline]
        pub const fn new() -> Self {
            Self {
                started: Mutex::new(Cell::new(false)),
                executor: UnsafeCell::new(MaybeUninit::uninit()),
            }
        }

        /// Executor interrupt callback.
        ///
        /// # Safety
        ///
        /// - You MUST call this from the interrupt handler, and from nowhere else.
        /// - You must not call this before calling `start()`.
        pub unsafe fn on_interrupt(&'static self) {
            let executor = unsafe { (&*self.executor.get()).assume_init_ref() };
            unsafe {
                executor.poll();
            }
        }

        /// Start the executor.
        ///
        /// This initializes the executor, stores the SGI it will be driven by, and returns.
        /// The executor keeps running in the background through the interrupt.
        ///
        /// This returns a [`SendSpawner`] you can use to spawn tasks on it. A [`SendSpawner`]
        /// is returned instead of a [`Spawner`](crate::Spawner) because the executor effectively runs in a
        /// different "thread" (the interrupt), so spawning tasks on it is effectively
        /// sending them.
        ///
        /// To obtain a [`Spawner`](crate::Spawner) for this executor, use [`Spawner::for_current_executor()`](crate::Spawner::for_current_executor()) from
        /// a task running in it.
        ///
        /// `sgi_interrupt_id` is the GIC Software-Generated Interrupt id (0..=15) used to
        /// drive this executor. When a task is woken, the executor pends this SGI on the
        /// requesting CPU.
        ///
        /// # Interrupt requirements
        ///
        /// You must write the interrupt handler yourself, and make it call [`on_interrupt()`](Self::on_interrupt)
        /// (after acknowledging the interrupt at the GIC CPU interface, and signalling
        /// end-of-interrupt afterwards).
        ///
        /// Unlike some other platforms, this method does NOT touch the GIC. You must
        /// configure the SGI (priority, group) and **enable** it on the GIC yourself,
        /// using your GIC driver of choice, before tasks are woken.
        ///
        /// [`SendSpawner`]: crate::SendSpawner
        pub fn start(&'static self, sgi_interrupt_id: u8) -> crate::SendSpawner {
            assert!(sgi_interrupt_id < 16, "SGI interrupt id must be in 0..=15");

            if critical_section::with(|cs| self.started.borrow(cs).replace(true)) {
                panic!("InterruptExecutor::start() called multiple times on the same executor.");
            }

            unsafe {
                (&mut *self.executor.get())
                    .as_mut_ptr()
                    .write(raw::Executor::new(sgi_interrupt_id as usize as *mut ()))
            }

            let executor = unsafe { (&*self.executor.get()).assume_init_ref() };

            executor.spawner().make_send()
        }

        /// Get a SendSpawner for this executor
        ///
        /// This returns a [`SendSpawner`](crate::SendSpawner) you can use to spawn tasks on this
        /// executor.
        ///
        /// This MUST only be called on an executor that has already been started.
        /// The function will panic otherwise.
        pub fn spawner(&'static self) -> crate::SendSpawner {
            if !critical_section::with(|cs| self.started.borrow(cs).get()) {
                panic!("InterruptExecutor::spawner() called on uninitialized executor.");
            }
            let executor = unsafe { (&*self.executor.get()).assume_init_ref() };
            executor.spawner().make_send()
        }
    }
}
