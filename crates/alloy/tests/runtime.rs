//! The shipped runtime, held to what a reader cannot see by reading it.

/// `allowed` closes over the rate limiter's state. Declared after that
/// function, the two names are globals inside it: the bucket lookup
/// indexes nil on the first rate-limited call, and the handler that
/// `on_ratelimited` sets never fires.
#[test]
fn the_rate_limiter_declares_its_state_before_it_reads_it() {
    for name in ["buckets", "on_limited"] {
        let first = alloy::RUNTIME
            .find(name)
            .unwrap_or_else(|| panic!("the runtime names `{name}`"));
        let declared = alloy::RUNTIME
            .find(&format!("local {name} "))
            .unwrap_or_else(|| panic!("the runtime declares `{name}`"));

        assert_eq!(
            first,
            declared + "local ".len(),
            "`{name}` is read before its `local`"
        );
    }
}
