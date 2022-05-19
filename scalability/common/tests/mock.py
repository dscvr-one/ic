"""A mock experiment."""
import sys
import unittest
from unittest import TestCase
from unittest.mock import MagicMock
from unittest.mock import Mock

import common.misc as misc
from common.base_experiment import BaseExperiment
from common.workload_experiment import WorkloadExperiment


class ExperimentMock(WorkloadExperiment):
    """Logic for experiment 1."""

    def __init__(self):
        """Construct experiment 1."""
        super().__init__()
        self.install_canister("some canister")

    def run_experiment_internal(self, config):
        """Mock similar to experiment 1."""
        return self.run_workload_generator(
            self.machines,
            self.target_nodes,
            200,
            outdir=self.iter_outdir,
            duration=60,
        )


class Test_Experiment(TestCase):
    """Implements a generic experiment with dependencies mocked away."""

    def test_verify__mock(self):
        """Test passes when the experiment runs to end."""
        sys.argv = [
            "mock.py",
            "--testnet",
            "abc",
            "--wg_testnet",
            "def",
            "--workload_generator_machines",
            "3.3.3.3,4.4.4.4",
        ]

        misc.parse_command_line_args()

        # Mock functions that won't work without a proper IC deployment
        ExperimentMock._WorkloadExperiment__get_targets = Mock(return_value=["1.1.1.1", "2.2.2.2"])
        ExperimentMock._WorkloadExperiment__get_subnet_for_target = MagicMock()
        ExperimentMock.get_subnet_to_instrument = MagicMock()
        BaseExperiment._get_subnet_info = Mock(return_value="{}")
        ExperimentMock._BaseExperiment__get_topology = Mock(return_value="{}")
        ExperimentMock._BaseExperiment__store_hardware_info = MagicMock()
        ExperimentMock.get_iter_logs_from_targets = MagicMock()
        ExperimentMock.install_canister = MagicMock()
        ExperimentMock.run_workload_generator = MagicMock()
        ExperimentMock._BaseExperiment__init_metrics = MagicMock()
        ExperimentMock._WorkloadExperiment__kill_workload_generator = MagicMock()
        BaseExperiment._turn_off_replica = MagicMock()
        ExperimentMock._WorkloadExperiment__check_workload_generator_installed = Mock(return_value=True)
        ExperimentMock.get_ic_version = MagicMock(return_value="deadbeef")
        ExperimentMock._WorkloadExperiment__wait_for_quiet = MagicMock(return_value=None)

        exp = ExperimentMock()
        exp.start_experiment()
        exp.run_experiment({})

        exp.subnet_id = "abc"
        exp.write_summary_file("test", {}, [], "some x value")
        exp.end_experiment()

        exp.install_canister.assert_called_once()
        exp.run_workload_generator.assert_called_once()
        exp._BaseExperiment__init_metrics.assert_called_once()


if __name__ == "__main__":
    unittest.main()
