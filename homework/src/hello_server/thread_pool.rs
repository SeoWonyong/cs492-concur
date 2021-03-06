//! Thread pool that joins all thread when dropped.

#![allow(clippy::mutex_atomic)]

// NOTE: Crossbeam channels are MPMC, which means that you don't need to wrap the receiver in
// Arc<Mutex<..>>. Just clone the receiver and give it to each worker thread.
use crossbeam_channel::{unbounded, Sender, Receiver};
use std::sync::{Arc, Condvar, Mutex};
use std::thread;

struct Job(Box<dyn FnOnce() + Send + 'static>);

#[derive(Debug)]
struct Worker {
    id: usize,
    thread: Option<thread::JoinHandle<()>>,
}

impl Drop for Worker {
    /// When dropped, the thread's `JoinHandle` must be `join`ed.  If the worker panics, then this
    /// function should panic too.  NOTE: that the thread is detached if not `join`ed explicitly.
    fn drop(&mut self) {
        self.thread.take().unwrap().join().unwrap();
    }
}

/// Internal data structure for tracking the current job status. This is shared by the worker
/// closures via `Arc` so that the workers can report to the pool that it started/finished a job.
#[derive(Debug, Default)]
struct ThreadPoolInner {
    job_count: Mutex<usize>,
    empty_condvar: Condvar,
}

impl ThreadPoolInner {
    /// Increment the job count.
    fn start_job(&self) {
        let mut count = self.job_count.lock().unwrap();
        *count += 1;
    }

    /// Decrement the job count.
    fn finish_job(&self) {
        let mut count = self.job_count.lock().unwrap();
        *count -= 1;
        self.empty_condvar.notify_all();
        
    }

    /// Wait until the job count becomes 0.
    ///
    /// NOTE: We can optimize this function by adding another field to `ThreadPoolInner`, but let's
    /// not care about that in this homework.
    fn wait_empty(&self) {
        let mut count = self.job_count.lock().unwrap();
        while *count != 0 {
            count = self.empty_condvar.wait(count).unwrap();
        }
    }
}

/// Thread pool.
#[derive(Debug)]
pub struct ThreadPool {
    workers: Vec<Worker>,
    job_sender: Option<Sender<Job>>,
    pool_inner: Arc<ThreadPoolInner>,
}

impl ThreadPool {
    /// Create a new ThreadPool with `size` threads. Panics if the size is 0.
    pub fn new(size: usize) -> Self {
        assert!(size > 0);

        let mut workers = Vec::with_capacity(size);

        let (job_sender, job_reciever): (Sender<Job>, Receiver<Job>) = unbounded();
        let job_reciever = Arc::new(job_reciever);

        let pool_inner = Arc::new(ThreadPoolInner {
            job_count: Mutex::new(0),
            empty_condvar: Condvar::new(),
        });

        for id in 0..size {
            let job_reciever = Arc::clone(&job_reciever);
            let pool_inner = pool_inner.clone();
            workers.push(Worker {
                id,
                thread: Some(thread::spawn(move || loop {
                    let job = job_reciever.recv();
                    match job {
                        Ok(job) => {
                            let job = job.0;
                            pool_inner.start_job();
                            job();
                            pool_inner.finish_job();
                        },
                        Err(_) => break,
                    }
                })),
            });
        }

        ThreadPool {
            workers,
            job_sender: Some(job_sender),
            pool_inner,
        }
    }

    /// Execute a new job in the thread pool.
    pub fn execute<F>(&self, f: F)
    where
        F: FnOnce() + Send + 'static,
    {
        let job = Job(Box::new(f));
        if let Some(sender) = &self.job_sender {
            sender.send(job).unwrap();
        }
    }

    /// Block the current thread until all jobs in the pool have been executed.  NOTE: This method
    /// has nothing to do with `JoinHandle::join`.
    pub fn join(&self) {
        if let  Some(sender) = &self.job_sender {
            loop {
                if sender.len() == 0 {
                    break;
                }
            }
        }
        self.pool_inner.wait_empty();
    }
}

impl Drop for ThreadPool {
    /// When dropped, all worker threads' `JoinHandle` must be `join`ed. If the thread panicked,
    /// then this function should panic too.
    fn drop(&mut self) {
        drop(self.job_sender.take().unwrap());
    }
}

#[cfg(test)]
mod test {
    use super::ThreadPool;
    use crossbeam_channel::bounded;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::{Arc, Barrier};
    use std::thread::sleep;
    use std::time::Duration;

    const NUM_THREADS: usize = 4;
    const NUM_JOBS: usize = 1024;

    #[test]
    fn thread_pool_parallel() {
        let pool = ThreadPool::new(NUM_THREADS);
        let barrier = Arc::new(Barrier::new(NUM_THREADS));
        let (done_sender, done_receiver) = bounded(NUM_THREADS);
        for _ in 0..NUM_THREADS {
            let barrier = barrier.clone();
            let done_sender = done_sender.clone();
            pool.execute(move || {
                barrier.wait();
                done_sender.send(()).unwrap();
            });
        }
        for _ in 0..NUM_THREADS {
            done_receiver.recv_timeout(Duration::from_secs(3)).unwrap();
        }
    }

    // Run jobs that take NUM_JOBS milliseconds as a whole.
    fn run_jobs(pool: &ThreadPool, counter: &Arc<AtomicUsize>) {
        for _ in 0..NUM_JOBS {
            let counter = counter.clone();
            pool.execute(move || {
                sleep(Duration::from_millis(NUM_THREADS as u64));
                counter.fetch_add(1, Ordering::Relaxed);
            });
        }
    }

    /// `join` blocks until all jobs are finished.
    #[test]
    fn thread_pool_join_block() {
        let pool = ThreadPool::new(NUM_THREADS);
        let counter = Arc::new(AtomicUsize::new(0));
        run_jobs(&pool, &counter);
        pool.join();
        assert_eq!(counter.load(Ordering::Relaxed), NUM_JOBS);
    }

    /// `drop` blocks until all jobs are finished.
    #[test]
    fn thread_pool_drop_block() {
        let pool = ThreadPool::new(NUM_THREADS);
        let counter = Arc::new(AtomicUsize::new(0));
        run_jobs(&pool, &counter);
        drop(pool);
        assert_eq!(counter.load(Ordering::Relaxed), NUM_JOBS);
    }

    /// This indirectly tests if the worker threads' `JoinHandle`s are joined when the pool is
    /// dropped.
    #[test]
    #[should_panic]
    fn thread_pool_drop_propagate_panic() {
        let pool = ThreadPool::new(NUM_THREADS);
        pool.execute(move || {
            panic!();
        });
    }
}
