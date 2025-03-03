use std::{
    fmt::Display,
    panic::{catch_unwind, UnwindSafe},
    path::{Path, PathBuf},
    time::{Duration, Instant},
};

use crate::driver::{
    farm::HostFeature,
    ic::{InternetComputer, VmAllocationStrategy, VmResources},
    test_env::TestEnv,
};
use serde::{Deserialize, Serialize};

pub trait PotSetupFn: FnOnce(TestEnv) + UnwindSafe + Send + Sync + 'static {}
impl<T: FnOnce(TestEnv) + UnwindSafe + Send + Sync + 'static> PotSetupFn for T {}

pub trait SysTestFn: FnOnce(TestEnv) + UnwindSafe + Send + Sync + 'static {}
impl<T: FnOnce(TestEnv) + UnwindSafe + Send + Sync + 'static> SysTestFn for T {}

pub fn suite(name: &str, pots: Vec<Pot>) -> Suite {
    let name = name.to_string();
    Suite {
        name,
        pots,
        alert_channels: vec![],
    }
}

pub fn pot_with_setup<F: PotSetupFn>(name: &str, setup: F, testset: TestSet) -> Pot {
    Pot::new(
        name,
        ExecutionMode::Run,
        setup,
        testset,
        None, // Pot timeout
        None, // VM allocation strategy
        None, // default VM resources
        vec![],
    )
}

pub fn pot(name: &str, mut ic: InternetComputer, testset: TestSet) -> Pot {
    pot_with_setup(
        name,
        move |env| ic.setup_and_start(&env).expect("failed to start IC"),
        testset,
    )
}

#[macro_export]
macro_rules! seq {
    ($($test:expr),+ $(,)?) => {
        TestSet::Sequence(vec![$(TestSet::from($test)),*])
    };
}
#[macro_export]
macro_rules! par {
    ($($test:expr),+ $(,)?) => {
        TestSet::Parallel(vec![$(TestSet::from($test)),*])
    };
}

pub fn seq(tests: Vec<Test>) -> TestSet {
    TestSet::Sequence(tests.into_iter().map(TestSet::Single).collect())
}

pub fn par(tests: Vec<Test>) -> TestSet {
    TestSet::Parallel(tests.into_iter().map(TestSet::Single).collect())
}

pub fn sys_t<F>(name: &str, test: F) -> Test
where
    F: SysTestFn,
{
    Test {
        name: name.to_string(),
        execution_mode: ExecutionMode::Run,
        f: Box::new(test),
    }
}

pub struct Pot {
    pub name: String,
    pub execution_mode: ExecutionMode,
    pub setup: ConfigState,
    pub testset: TestSet,
    pub pot_timeout: Option<Duration>,
    pub vm_allocation: Option<VmAllocationStrategy>,
    pub default_vm_resources: Option<VmResources>,
    pub required_host_features: Vec<HostFeature>,
    pub alert_channels: Vec<SlackChannel>,
}

// In order to evaluate this function in a catch_unwind(), we need to take
// ownership of it and thus move it out of the object.
#[allow(clippy::large_enum_variant)]
pub enum ConfigState {
    Function(Box<dyn PotSetupFn>),
    Evaluated(std::thread::Result<()>),
}

impl ConfigState {
    pub fn evaluate(&mut self, test_env: TestEnv) -> &std::thread::Result<()> {
        fn dummy(_: TestEnv) {
            unimplemented!()
        }
        let mut tmp = Self::Function(Box::new(dummy));
        std::mem::swap(&mut tmp, self);
        tmp = match tmp {
            ConfigState::Function(f) => ConfigState::Evaluated(catch_unwind(move || f(test_env))),
            r @ ConfigState::Evaluated(_) => r,
        };
        std::mem::swap(&mut tmp, self);
        if let Self::Evaluated(r) = self {
            return r;
        }
        unreachable!()
    }
}

impl Pot {
    pub fn new<F: PotSetupFn>(
        name: &str,
        execution_mode: ExecutionMode,
        config: F,
        testset: TestSet,
        pot_timeout: Option<Duration>,
        default_vm_resources: Option<VmResources>,
        vm_allocation: Option<VmAllocationStrategy>,
        required_host_features: Vec<HostFeature>,
    ) -> Self {
        Self {
            name: name.to_string(),
            execution_mode,
            setup: ConfigState::Function(Box::new(config)),
            testset,
            pot_timeout,
            vm_allocation,
            default_vm_resources,
            required_host_features,
            alert_channels: vec![],
        }
    }

    pub fn with_ttl(mut self, time_limit: Duration) -> Self {
        self.pot_timeout = Some(time_limit);
        self
    }

    pub fn with_vm_allocation(mut self, vm_allocation: VmAllocationStrategy) -> Self {
        self.vm_allocation = Some(vm_allocation);
        self
    }

    pub fn with_required_host_features(mut self, required_host_features: Vec<HostFeature>) -> Self {
        self.required_host_features = required_host_features;
        self
    }

    /// Set the VM resources (like number of virtual CPUs and memory) of all
    /// implicitly constructed nodes.
    ///
    /// Setting the VM resources for explicitly constructed nodes
    /// has to be via `Node::new_with_vm_resources`.
    pub fn with_default_vm_resources(mut self, default_vm_resources: Option<VmResources>) -> Self {
        self.default_vm_resources = default_vm_resources;
        self
    }
}

pub enum TestSet {
    Sequence(Vec<TestSet>),
    Parallel(Vec<TestSet>),
    Single(Test),
}

impl From<Test> for TestSet {
    fn from(t: Test) -> Self {
        TestSet::Single(t)
    }
}

impl TestSet {
    pub fn iter(&self) -> impl Iterator<Item = &Test> {
        enum Iter<'a> {
            Single(Option<&'a Test>),
            Vec(Vec<std::slice::Iter<'a, TestSet>>),
        }
        impl<'a> Iterator for Iter<'a> {
            type Item = &'a Test;
            fn next(&mut self) -> Option<Self::Item> {
                let vec = match self {
                    Iter::Single(test) => return test.take(),
                    Iter::Vec(vec) => vec,
                };
                loop {
                    use TestSet::*;
                    let mut ti = vec.pop()?;
                    let next = match ti.next() {
                        None => continue,
                        Some(next) => next,
                    };
                    vec.push(ti);
                    match next {
                        Single(test) => return Some(test),
                        Parallel(tests) | Sequence(tests) => vec.push(tests.iter()),
                    }
                }
            }
        }

        match self {
            Self::Single(test) => Iter::Single(Some(test)),
            Self::Parallel(tests) | Self::Sequence(tests) => Iter::Vec(vec![tests.iter()]),
        }
    }

    pub fn iter_mut(&mut self) -> impl Iterator<Item = &mut Test> {
        enum IterMut<'a> {
            Single(Option<&'a mut Test>),
            Vec(Vec<std::slice::IterMut<'a, TestSet>>),
        }
        impl<'a> Iterator for IterMut<'a> {
            type Item = &'a mut Test;
            fn next(&mut self) -> Option<Self::Item> {
                let vec = match self {
                    IterMut::Single(test) => return test.take(),
                    IterMut::Vec(vec) => vec,
                };
                loop {
                    use TestSet::*;
                    let mut ti = vec.pop()?;
                    let next = match ti.next() {
                        None => continue,
                        Some(next) => next,
                    };
                    vec.push(ti);
                    match next {
                        Single(test) => return Some(test),
                        Parallel(tests) | Sequence(tests) => vec.push(tests.iter_mut()),
                    }
                }
            }
        }

        match self {
            Self::Single(test) => IterMut::Single(Some(test)),
            Self::Parallel(tests) | Self::Sequence(tests) => IterMut::Vec(vec![tests.iter_mut()]),
        }
    }
}

pub struct Test {
    pub name: String,
    pub execution_mode: ExecutionMode,
    pub f: Box<dyn SysTestFn>,
}

pub struct Suite {
    pub name: String,
    pub pots: Vec<Pot>,
    pub alert_channels: Vec<SlackChannel>,
}

#[derive(Debug, Deserialize, Serialize)]
/// A tree-like structure containing execution plan of the test suite.
pub struct TestSuiteContract {
    pub name: String,
    pub is_skipped: bool,
    pub alert_channels: Vec<SlackChannel>,
    pub children: Vec<TestSuiteContract>,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub enum TestResult {
    Passed,
    Failed(String),
    Skipped,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
/// A tree-like structure containing statistics on how much time it took to
/// complete a node and all its children, i.e. threads spawned from the node.
pub struct TestResultNode {
    pub name: String,
    #[serde(with = "serde_millis")]
    pub started_at: Instant,
    pub duration: Duration,
    pub result: TestResult,
    pub children: Vec<TestResultNode>,
    pub alert_channels: Vec<SlackChannel>,
}

impl Default for TestResultNode {
    fn default() -> Self {
        Self {
            name: String::default(),
            started_at: Instant::now(),
            duration: Duration::default(),
            result: TestResult::Skipped,
            children: vec![],
            alert_channels: vec![],
        }
    }
}

impl TestResult {
    pub fn failed_with_message(message: &str) -> TestResult {
        TestResult::Failed(message.to_string())
    }
}

impl From<&TestSuiteContract> for TestResultNode {
    fn from(contract: &TestSuiteContract) -> Self {
        let result = if contract.is_skipped {
            TestResult::Skipped
        } else {
            TestResult::Failed("".to_string())
        };
        Self {
            name: contract.name.clone(),
            children: contract.children.iter().map(TestResultNode::from).collect(),
            result,
            alert_channels: contract.alert_channels.clone(),
            ..Default::default()
        }
    }
}

pub fn infer_parent_result(children: &[TestResultNode]) -> TestResult {
    if children.iter().all(|t| t.result == TestResult::Skipped) {
        return TestResult::Skipped;
    }
    if children
        .iter()
        .any(|t| matches!(t.result, TestResult::Failed(_)))
    {
        TestResult::failed_with_message("")
    } else {
        TestResult::Passed
    }
}

pub fn propagate_children_results_to_parents(root: &mut TestResultNode) {
    if root.children.is_empty() {
        return;
    }
    root.children
        .iter_mut()
        .for_each(propagate_children_results_to_parents);
    root.result = infer_parent_result(&root.children);
    root.started_at = root
        .children
        .iter()
        .map(|child| child.started_at)
        .min()
        .unwrap();
    root.duration = root
        .children
        .iter()
        .map(|child| child.duration)
        .max()
        .unwrap();
}

pub trait Alertable {
    fn with_alert<T: Into<SlackChannel>>(self, channel: T) -> Self;
}

#[derive(Debug, Clone, Serialize, Deserialize, Eq, PartialEq)]
pub struct SlackChannel(String);

impl From<&str> for SlackChannel {
    fn from(name: &str) -> Self {
        Self(name.to_string())
    }
}

#[derive(Serialize)]
pub struct SlackAlert {
    channel: SlackChannel,
    message: String,
}

impl SlackAlert {
    pub fn new(channel: SlackChannel, message: String) -> Self {
        Self { channel, message }
    }
}

impl Alertable for Suite {
    fn with_alert<T: Into<SlackChannel>>(mut self, channel: T) -> Self {
        self.alert_channels.push(channel.into());
        self
    }
}

impl Alertable for Pot {
    fn with_alert<T: Into<SlackChannel>>(mut self, channel: T) -> Self {
        self.alert_channels.push(channel.into());
        self
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize)]
pub enum ExecutionMode {
    Run,
    Skip,
    Ignore,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
pub struct TestPath(Vec<String>);

impl Display for TestPath {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.0.join("::"))
    }
}

impl TestPath {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn to_filepath(&self, base_dir: &Path) -> PathBuf {
        let filepath = PathBuf::new();
        filepath.join(base_dir).join(self.0.join("/"))
    }

    pub fn new_with_root<S: ToString>(root: S) -> Self {
        Self(vec![root.to_string()])
    }

    pub fn url_string(&self) -> String {
        self.0.join("__")
    }

    pub fn join<S: ToString>(&self, p: S) -> TestPath {
        let p = p.to_string();
        if !Self::is_c_like_ident(&p) {
            panic!("Invalid identifiers (must be c-like): {}", p);
        }
        let mut copy = self.0.clone();
        copy.push(p);
        TestPath(copy)
    }

    pub fn pop(&mut self) -> String {
        self.0.pop().expect("cannot pop from empty testpath")
    }

    pub fn is_c_like_ident(s: &str) -> bool {
        if s.is_empty() {
            return false;
        }

        let first = s.chars().next().unwrap();
        if !(first.is_ascii_alphabetic() || first == '_') {
            return false;
        }

        for c in s.chars().skip(1) {
            if !(c.is_ascii_alphanumeric() || c == '_') {
                return false;
            }
        }

        true
    }
}

#[cfg(test)]
mod tests {
    use slog::{o, Logger};

    use super::*;

    #[test]
    fn config_can_be_lazily_evaluated() {
        let mut config_state = ConfigState::Function(Box::new(|_| {}));
        let logger = Logger::root(slog::Discard, o!());
        let tempdir = tempfile::tempdir().unwrap();
        config_state.evaluate(TestEnv::new(tempdir.path(), logger).unwrap());
    }

    #[test]
    fn failing_config_evaluation_can_be_caught() {
        let tempdir = tempfile::tempdir().unwrap();
        let mut config_state = ConfigState::Function(Box::new(|_| panic!("magic error!")));
        let logger = Logger::root(slog::Discard, o!());
        let e = config_state
            .evaluate(TestEnv::new(tempdir.path(), logger).unwrap())
            .as_ref()
            .unwrap_err();
        if let Some(s) = e.downcast_ref::<&str>() {
            assert!(s.contains("magic error!"));
        } else {
            panic!("Error is not string")
        }
    }
}
