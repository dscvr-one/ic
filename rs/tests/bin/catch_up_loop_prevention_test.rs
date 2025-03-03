#[rustfmt::skip]

use anyhow::Result;

use ic_tests::consensus::catch_up_test::{catch_up_loop, test_catch_up_possible};
use ic_tests::driver::new::group::SystemTestGroup;
use ic_tests::systest;
use std::time::Duration;

const TIMEOUT: Duration = Duration::from_secs(20 * 60);

fn main() -> Result<()> {
    SystemTestGroup::new()
        .with_setup(catch_up_loop)
        .add_test(systest!(test_catch_up_possible))
        .with_timeout_per_test(TIMEOUT)
        .with_overall_timeout(TIMEOUT)
        .execute_from_args()?;

    Ok(())
}
