use std::sync::{
    atomic::{AtomicIsize, AtomicPtr, Ordering},
    Mutex,
};

/// A generation of RCU data that we're retiring as soon as its ref count hits 0
struct Retiring {
    refs: Box<AtomicIsize>,
    deleter: Box<dyn Fn()>,
}

impl Drop for Retiring {
    fn drop(&mut self) {
        while self.refs.load(Ordering::Acquire) > 0 {
            // Wait until nobody is referring to us.
            // < Gib futex >
        }
        (self.deleter)();
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

impl Domain {
    pub fn new() -> Self {
        Self {
            refs: AtomicPtr::new(Box::into_raw(Box::new(AtomicIsize::new(0)))),
            to_retire: Mutex::new(vec![]),
        }
    }

    pub fn read_lock(&self) -> ReadLock {
        let current_gen = self.refs.load(Ordering::Acquire);
        assert!(!current_gen.is_null());
        ReadLock::new(current_gen)
    }

    pub fn retire<T: 'static>(&self, p: *mut T) {
        let mut retirees = self.to_retire.lock().unwrap();

        // We're retiring this generation.
        let previous_generation = self.refs.swap(
            Box::into_raw(Box::new(AtomicIsize::new(0))),
            Ordering::SeqCst, // relaxed since we have a lock?
        );
        let ret = unsafe { Retiring {
            refs: Box::from_raw(previous_generation),
            deleter: Box::new(move || { Box::from_raw(p); })
        }};

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

    use rand::Rng;
    use std::{
        sync::{
            atomic::{AtomicPtr, Ordering},
            Arc,
        },
        thread, time,
    };

    #[test]
    fn smoke() {
        let mut handles = vec![];

        // Should probably wrap this in something nice that takes Box
        let rcu_ptr = AtomicPtr::default();
        let domain = Arc::new(Domain::new());

        let wd = domain.clone();
        let writer = thread::spawn(move || {
            for i in 0..10 {
                thread::sleep(time::Duration::from_millis(10));
                let new = Box::into_raw(Box::new(i as isize));
                let old = rcu_ptr.swap(new, Ordering::SeqCst);
                println!("Wrote new {:?}, retiring old {:?}", new, old);
                wd.retire(old);
            }
        });

        // reader threads
        for i in 0..10 {
            let rd = domain.clone();
            let handle = thread::spawn(move || {
                for _ in 0..10 {
                    let mut rng = rand::thread_rng();
                    thread::sleep(time::Duration::from_millis(rng.gen_range(1..10)));
                    let rl = rd.read_lock();
                    let d = rcu_ptr.load(Ordering::Acquire);
                    println!("Reader {}: data {:?}", i, d);
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
