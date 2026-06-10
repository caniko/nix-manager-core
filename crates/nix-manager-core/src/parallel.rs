use std::sync::Mutex;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::thread;

/// Run `f` over `items` with at most `max_workers` threads in flight.
/// Output order matches input order. Each item is processed exactly once.
///
/// Bounded by `min(max_workers, items.len())`. Uses `thread::scope` so the
/// closure may borrow from the caller's stack.
pub fn parallel_map<T, R, F>(items: Vec<T>, max_workers: usize, f: F) -> Vec<R>
where
    T: Send,
    R: Send,
    F: Fn(T) -> R + Send + Sync,
{
    let n = items.len();
    if n == 0 {
        return Vec::new();
    }

    let inputs: Vec<Mutex<Option<T>>> = items.into_iter().map(|t| Mutex::new(Some(t))).collect();
    let outputs: Vec<Mutex<Option<R>>> = (0..n).map(|_| Mutex::new(None)).collect();
    let next = AtomicUsize::new(0);
    let workers = max_workers.max(1).min(n);

    let inputs_ref = &inputs;
    let outputs_ref = &outputs;
    let next_ref = &next;
    let f_ref = &f;

    thread::scope(|s| {
        for _ in 0..workers {
            s.spawn(move || {
                loop {
                    let i = next_ref.fetch_add(1, Ordering::Relaxed);
                    if i >= n {
                        return;
                    }
                    let item = inputs_ref[i].lock().unwrap().take().unwrap();
                    let r = f_ref(item);
                    *outputs_ref[i].lock().unwrap() = Some(r);
                }
            });
        }
    });

    outputs
        .into_iter()
        .map(|m| m.into_inner().unwrap().unwrap())
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::AtomicUsize;
    use std::time::Duration;

    #[test]
    fn preserves_input_order() {
        let out = parallel_map((0..50).collect(), 8, |x: i32| x * 2);
        let expected: Vec<i32> = (0..50).map(|x| x * 2).collect();
        assert_eq!(out, expected);
    }

    #[test]
    fn handles_empty_input() {
        let out: Vec<i32> = parallel_map(Vec::<i32>::new(), 4, |x| x);
        assert!(out.is_empty());
    }

    #[test]
    fn each_item_processed_once() {
        let counter = AtomicUsize::new(0);
        let n = 100;
        let _: Vec<()> = parallel_map((0..n).collect::<Vec<_>>(), 16, |_| {
            counter.fetch_add(1, Ordering::Relaxed);
        });
        assert_eq!(counter.load(Ordering::Relaxed), n);
    }

    #[test]
    fn actually_runs_in_parallel() {
        let start = std::time::Instant::now();
        let _: Vec<()> = parallel_map((0..8).collect::<Vec<_>>(), 4, |_| {
            std::thread::sleep(Duration::from_millis(100));
        });
        let elapsed = start.elapsed();
        assert!(
            elapsed < Duration::from_millis(500),
            "expected parallel execution; took {elapsed:?}"
        );
    }

    #[test]
    fn workers_capped_at_item_count() {
        let out = parallel_map(vec![1, 2, 3], 100, |x: i32| x + 10);
        assert_eq!(out, vec![11, 12, 13]);
    }

    #[test]
    fn single_item_single_worker_works() {
        let out = parallel_map(vec![42], 1, |x: i32| x * 2);
        assert_eq!(out, vec![84]);
    }

    #[test]
    fn zero_workers_clamps_to_one() {
        let out = parallel_map(vec![1, 2, 3], 0, |x: i32| x + 1);
        assert_eq!(out, vec![2, 3, 4]);
    }

    #[test]
    fn results_carry_per_item_errors() {
        let out: Vec<Result<i32, &'static str>> = parallel_map((0..6).collect(), 3, |x: i32| {
            if x % 2 == 0 { Ok(x * 10) } else { Err("odd") }
        });
        let oks: Vec<i32> = out
            .iter()
            .filter_map(|r| r.as_ref().ok().copied())
            .collect();
        let errs = out.iter().filter(|r| r.is_err()).count();
        assert_eq!(oks, vec![0, 20, 40]);
        assert_eq!(errs, 3);
    }

    #[test]
    fn closure_can_borrow_caller_state() {
        let counter = AtomicUsize::new(100);
        let prefix = String::from("item-");
        let out: Vec<String> = parallel_map((0..4).collect(), 2, |x: i32| {
            counter.fetch_add(1, Ordering::Relaxed);
            format!("{prefix}{x}")
        });
        assert_eq!(out, vec!["item-0", "item-1", "item-2", "item-3"]);
        assert_eq!(counter.load(Ordering::Relaxed), 104);
    }
}
