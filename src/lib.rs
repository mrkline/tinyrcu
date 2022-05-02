use std::sync::{
    atomic::{AtomicIsize, AtomicPtr, Ordering},
    Mutex,
};

/// A generation of RCU data that we're retiring as soon as its ref count hits 0
struct Retiring {
    refs: Box<AtomicIsize>,
    _deleter: Box<dyn Send>,
}

impl Drop for Retiring {
    fn drop(&mut self) {
        while self.refs.load(Ordering::Acquire) > 0 {
            // Wait until nobody is referring to us.
            // < Gib futex >
        }
    }
}

/// A read lock on a given RCU domain
pub struct ReadLock {
    refs: *const AtomicIsize,
}

impl ReadLock {
    fn new(r: *const AtomicIsize) -> Self {
        unsafe {
            (*r).fetch_add(1, Ordering::Relaxed);
        }
        Self { refs: r }
    }
}

impl Drop for ReadLock {
    fn drop(&mut self) {
        unsafe {
            (*self.refs).fetch_sub(1, Ordering::AcqRel);
        }
    }
}

/// An RCU garbage collector^W^W domain
pub struct Domain {
    refs: AtomicPtr<AtomicIsize>,
    to_retire: Mutex<Vec<Retiring>>,
}

impl Default for Domain {
    fn default() -> Self {
        Self {
            refs: AtomicPtr::new(Box::into_raw(Box::new(AtomicIsize::new(0)))),
            to_retire: Mutex::new(vec![]),
        }
    }
}

impl Domain {
    pub fn read_lock(&self) -> ReadLock {
        let current_gen = self.refs.load(Ordering::Acquire);
        assert!(!current_gen.is_null());
        ReadLock::new(current_gen)
    }

    pub unsafe fn retire<T: 'static + Send>(&self, p: *mut T) {
        let mut retirees = self.to_retire.lock().unwrap();

        // We're retiring this generation.
        let previous_generation = self.refs.swap(
            Box::into_raw(Box::new(AtomicIsize::new(0))),
            Ordering::SeqCst, // relaxed since we have a lock?
        );

        let deleter = Box::from_raw(p);

        let ret = Retiring {
            refs: Box::from_raw(previous_generation),
            _deleter: deleter,
        };

        // Add the retired generation to the list and delete all the ones
        // which don't have any references anymore.
        retirees.push(ret);
        retirees.retain(|r| r.refs.load(Ordering::Acquire) > 0);
    }
}

impl Drop for Domain {
    fn drop(&mut self) {
        unsafe { Box::from_raw(self.refs.load(Ordering::Relaxed)) };
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    use lazy_static::lazy_static;
    use rand::Rng;
    use std::{
        sync::atomic::{AtomicPtr, Ordering},
        thread, time,
    };

    #[test]
    fn smoke() {
        let mut handles = vec![];

        // Should probably wrap this in something nice that takes Box
        lazy_static! {
            static ref RCU_PTR: AtomicPtr<isize> = AtomicPtr::default();
            static ref DOM: Domain = Domain::default();
        };

        let writer = thread::spawn(move || {
            for i in 0..10 {
                let new = Box::into_raw(Box::new(i as isize));
                let old = RCU_PTR.swap(new, Ordering::SeqCst);
                println!("Wrote new {:?}, retiring old {:?}", new, old);
                unsafe {
                    DOM.retire(old);
                }
                thread::sleep(time::Duration::from_millis(5));
            }
        });

        // reader threads
        for i in 0..10 {
            let handle = thread::spawn(move || {
                for _ in 0..10 {
                    let mut rng = rand::thread_rng();
                    thread::sleep(time::Duration::from_millis(rng.gen_range(1..10)));
                    let _rl = DOM.read_lock();
                    let d = RCU_PTR.load(Ordering::Acquire);
                    println!("Reader {}: data {:?}", i, unsafe { d.as_ref() });
                }
            });
            handles.push(handle);
        }

        for handle in handles {
            handle.join().unwrap();
        }

        writer.join().unwrap();
    }
}
