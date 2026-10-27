//! Scoped cycle counters for performance work (`--features prof`): `prof!(NAME)` at the top of
//! a block adds that block's call count and inclusive TSC cycles to a per-thread table, which
//! `bench` prints per decision step. Without the feature the macro expands to nothing. Each
//! scope costs about 8 ns, so small functions read high; compare calls and large scopes.

#[cfg(feature = "prof")]
mod imp {
    use std::cell::RefCell;

    pub const MAX: usize = 128;

    thread_local! {
        static TABLE: RefCell<([&'static str; MAX], [(u64, u64); MAX], usize)> =
            const { RefCell::new(([""; MAX], [(0, 0); MAX], 0)) };
    }

    #[inline(always)]
    fn now() -> u64 {
        // SAFETY: rdtsc has no preconditions on x86-64.
        unsafe { core::arch::x86_64::_rdtsc() }
    }

    pub struct Scope {
        slot: usize,
        t0: u64,
    }

    impl Scope {
        #[inline(always)]
        pub fn new(slot: &'static std::sync::atomic::AtomicUsize, name: &'static str) -> Scope {
            use std::sync::atomic::Ordering;
            let mut k = slot.load(Ordering::Relaxed);
            if k == usize::MAX {
                k = TABLE.with(|t| {
                    let mut t = t.borrow_mut();
                    let k = (0..t.2).find(|&i| t.0[i] == name).unwrap_or_else(|| {
                        let k = t.2;
                        assert!(k < MAX, "prof: too many scopes");
                        t.0[k] = name;
                        t.2 += 1;
                        k
                    });
                    k
                });
                slot.store(k, Ordering::Relaxed);
            }
            Scope { slot: k, t0: now() }
        }
    }

    impl Drop for Scope {
        #[inline(always)]
        fn drop(&mut self) {
            let dt = now() - self.t0;
            TABLE.with(|t| {
                let mut t = t.borrow_mut();
                t.1[self.slot].0 += 1;
                t.1[self.slot].1 += dt;
            });
        }
    }

    /// (name, calls, cycles) of every scope seen on this thread, in first-seen order.
    pub fn report() -> Vec<(&'static str, u64, u64)> {
        TABLE.with(|t| {
            let t = t.borrow();
            (0..t.2).map(|i| (t.0[i], t.1[i].0, t.1[i].1)).collect()
        })
    }

    pub fn reset() {
        TABLE.with(|t| {
            let mut t = t.borrow_mut();
            t.1 = [(0, 0); MAX];
        });
    }
}

#[cfg(feature = "prof")]
pub use imp::{report, reset, Scope};

/// Count and time the rest of the enclosing block under `name` (only with `--features prof`).
#[macro_export]
macro_rules! prof {
    ($name:expr) => {
        #[cfg(feature = "prof")]
        let _prof_scope = {
            static SLOT: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(usize::MAX);
            $crate::prof::Scope::new(&SLOT, $name)
        };
    };
}
