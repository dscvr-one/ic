#[rustfmt::skip]

use anyhow::Result;

use ic_tests::driver::new::group::SystemTestGroup;
use ic_tests::message_routing::xnet_slo_test::Config;
use ic_tests::systest;
use std::time::Duration;

const SUBNETS: usize = 29;
const NODES_PER_SUBNET: usize = 1;
const RUNTIME: Duration = Duration::from_secs(600);
const REQUEST_RATE: usize = 10;

const PER_TASK_TIMEOUT: Duration = Duration::from_secs(25 * 60);
const OVERALL_TIMEOUT: Duration = Duration::from_secs(35 * 60);

fn main() -> Result<()> {
    let config = Config::new(SUBNETS, NODES_PER_SUBNET, RUNTIME, REQUEST_RATE);
    let test = config.clone().test();
    SystemTestGroup::new()
        .with_setup(config.build())
        .add_test(systest!(test))
        .with_timeout_per_test(PER_TASK_TIMEOUT) // each task (including the setup function) may take up to `per_task_timeout`.
        .with_overall_timeout(OVERALL_TIMEOUT) // the entire group may take up to `overall_timeout`.
        .execute_from_args()?;
    Ok(())
}
