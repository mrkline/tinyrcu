use std::sync::{
    atomic::{AtomicIsize, Ordering},
    Arc, Mutex,
};

/// A generation of RCU data that we're retiring as soon as its ref count hits 0
struct Retiring {
    refs: Arc<AtomicIsize>, // arc so that the domain and read locks can share
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
    // Instead of an arc, could we make this a pin, owned by the Domain?
    // We're positive the Domain outlives ReadLocks on it (or they'd better!)
    // Would save us bumping the ref count just to bump the ref count.
    refs: Arc<AtomicIsize>,
}

impl ReadLock {
    fn new(r: Arc<AtomicIsize>) -> Self {
        r.fetch_add(1, Ordering::Relaxed);
        Self { refs: r }
    }
}

impl Drop for ReadLock {
    fn drop(&mut self) {
        self.refs.fetch_sub(1, Ordering::AcqRel);
    }
}

/// An RCU garbage collector^W^W domain
pub struct Domain {
    refs: Arc<AtomicIsize>,
    to_retire: Vec<Retiring>,
}

impl Domain {
    pub fn new() -> Self {
        Self {
            refs: Arc::new(AtomicIsize::new(0)),
            to_retire: vec![],
        }
    }

    pub fn read_lock(&self) -> ReadLock {
        ReadLock::new(self.refs.clone())
    }

    pub fn retire<T: 'static>(&self, p: *mut T) {
        // We're retiring this generation.
        let ret = Retiring {
            refs: self.refs.clone(),
            deleter: Box::new(move || unsafe {
                Box::from_raw(p);
            }),
        };

        // Start a new generation
        self.refs = Arc::new(AtomicIsize::new(0));

        // Add the retired generation to the list and delete all the ones
        // which don't have any references anymore.
        self.to_retire.push(ret);
        self.to_retire
            .retain(|r| r.refs.load(Ordering::Acquire) > 0);
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
