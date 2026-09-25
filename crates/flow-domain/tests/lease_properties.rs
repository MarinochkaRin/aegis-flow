use std::time::{Duration, UNIX_EPOCH};

use flow_domain::{Lease, LeaseToken};
use proptest::prelude::*;

proptest! {
    #[test]
    fn valid_renewal_never_shortens_deadline(
        elapsed_secs in 0_u64..30,
        extension_secs in 1_u64..3600,
    ) {
        let token = LeaseToken::new();
        let start = UNIX_EPOCH + Duration::from_secs(10_000);
        let mut lease = Lease::new(token, start, Duration::from_secs(60)).unwrap();
        let before = lease.expires_at();

        lease.renew(
            &token,
            start + Duration::from_secs(elapsed_secs),
            Duration::from_secs(extension_secs),
        ).unwrap();

        prop_assert!(lease.expires_at() >= before);
    }
}
