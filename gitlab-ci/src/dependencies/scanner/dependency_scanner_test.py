import datetime
import typing
from unittest.mock import Mock
from unittest.mock import patch

import pytest
from data_source.jira_finding_data_source import JiraFinding
from model.dependency import Dependency
from model.finding import Finding
from model.repository import Project
from model.repository import Repository
from model.security_risk import SecurityRisk
from model.user import User
from model.vulnerability import Vulnerability
from scanner.dependency_manager import Bazel
from scanner.dependency_scanner import BazelICScanner


@pytest.fixture
def jira_lib_mock():
    return Mock()


class FakeBazel(Bazel):
    def __init__(self, fake_type: int):
        super().__init__()
        self.fake_type = fake_type

    def get_findings(self, repository: Repository, scanner: str) -> typing.List[Finding]:
        if self.fake_type == 1:
            return []

        if self.fake_type == 2:
            return [
                Finding(
                    repository=repository.name,
                    scanner=scanner,
                    vulnerable_dependency=Dependency("VDID1", "chrono", "1.0", {"VID1": ["1.1", "2.0"]}),
                    vulnerabilities=[Vulnerability("VID1", "CVE-123", "huuughe vuln", 100)],
                    first_level_dependencies=[Dependency("VDID2", "fl dep", "0.1 beta", {"VID1": ["3.0 alpha"]})],
                    projects=["foo", "bar", "bear"],
                    risk_assessor=[],
                    score=100,
                )
            ]

        if self.fake_type == 3:
            return [
                Finding(
                    repository=repository.name,
                    scanner=scanner,
                    vulnerable_dependency=Dependency(
                        "VDID1", "chrono", "1.0", {"VID1": ["1.1", "2.0"], "VID2": ["1.1", "2.0"]}
                    ),
                    vulnerabilities=[
                        Vulnerability("VID1", "CVE-123", "huuughe vuln", 100),
                        Vulnerability("VID2", "CVE-456", "CRITICAL VULN o.O", 120),
                    ],
                    first_level_dependencies=[
                        Dependency("VDID2", "fl dep", "0.1 beta", {"VID1": ["3.0 alpha"]}),
                        Dependency("VDID3", "fal dep", "0.2 beta", {"VID1": ["3.0 alpha"]}),
                    ],
                    projects=["foo", "bar", "bear", "new foo", "new bear"],
                    risk_assessor=[],
                    score=120,
                )
            ]


def test_on_periodic_job_no_findings(jira_lib_mock):
    # No findings
    sub1 = Mock()
    sub2 = Mock()
    scanner_job = BazelICScanner(FakeBazel(1), jira_lib_mock, [sub1, sub2])

    scanner_job.on_periodic_scan()

    jira_lib_mock.get_open_finding.assert_not_called()
    jira_lib_mock.create_or_update_open_finding.assert_not_called()
    sub1.on_scan_job_succeeded.assert_called_once()
    sub2.on_scan_job_succeeded.assert_called_once()
    sub1.on_scan_job_failed.assert_not_called()
    sub2.on_scan_job_failed.assert_not_called()


def test_on_periodic_job_one_finding(jira_lib_mock):
    # one finding, not present in JIRA
    jira_lib_mock.get_open_finding.return_value = []
    jira_lib_mock.get_risk_assessor.return_value = [User("mickey", "Mickey Mouse")]

    sub1 = Mock()
    sub2 = Mock()
    fake_bazel = FakeBazel(2)
    scanner_job = BazelICScanner(fake_bazel, jira_lib_mock, [sub1, sub2])

    scanner = "BAZEL_IC"
    repository = Repository("ic", "https://gitlab.com/dfinity-lab/public/ic", [Project("ic", "ic")])
    finding = fake_bazel.get_findings(repository, scanner)[0]
    finding.risk_assessor = [User("mickey", "Mickey Mouse")]

    scanner_job.on_periodic_scan()

    jira_lib_mock.get_open_finding.assert_called_once()
    jira_lib_mock.get_risk_assessor.assert_called_once()

    jira_lib_mock.create_or_update_open_finding.assert_called_once()
    jira_lib_mock.create_or_update_open_finding.assert_called_once_with(finding)

    sub1.on_scan_job_succeeded.assert_called_once()
    sub2.on_scan_job_succeeded.assert_called_once()
    sub1.on_scan_job_failed.assert_not_called()
    sub2.on_scan_job_failed.assert_not_called()


def test_on_periodic_job_one_finding_in_jira(jira_lib_mock):
    # one finding, present in JIRA
    scanner = "BAZEL_IC"
    repository = Repository("ic", "https://gitlab.com/dfinity-lab/public/ic", [Project("ic", "ic")])
    jira_finding = JiraFinding(
        repository=repository.name,
        scanner=scanner,
        vulnerable_dependency=Dependency("VDID1", "chrono", "1.0", {"VID1": ["1.1", "2.0"]}),
        vulnerabilities=[Vulnerability("VID1", "CVE-123", "huuughe vuln", 100)],
        first_level_dependencies=[Dependency("VDID2", "fl dep", "0.1 beta", {"VID1": ["3.0 alpha"]})],
        projects=["foo", "bar", "bear"],
        risk_assessor=[User("mickey", "Mickey Mouse")],
        risk=SecurityRisk.INFORMATIONAL,
        patch_responsible=[],
        due_date=100,
        score=100,
    )
    jira_lib_mock.get_open_finding.return_value = jira_finding

    sub1 = Mock()
    sub2 = Mock()
    fake_bazel = FakeBazel(2)
    scanner_job = BazelICScanner(fake_bazel, jira_lib_mock, [sub1, sub2])
    scanner_job.on_periodic_scan()

    finding = fake_bazel.get_findings(repository, scanner)[0]
    jira_finding.vulnerable_dependency = finding.vulnerable_dependency
    jira_finding.vulnerabilities = finding.vulnerabilities
    jira_finding.first_level_dependencies = finding.first_level_dependencies
    jira_finding.projects = finding.projects
    jira_finding.risk = None
    jira_finding.score = finding.score

    jira_lib_mock.get_open_finding.assert_called_once()
    jira_lib_mock.get_risk_assessor.assert_not_called()

    jira_lib_mock.create_or_update_open_finding.assert_called_once()
    jira_lib_mock.create_or_update_open_finding.assert_called_once_with(jira_finding)

    sub1.on_scan_job_succeeded.assert_called_once()
    sub2.on_scan_job_succeeded.assert_called_once()
    sub1.on_scan_job_failed.assert_not_called()
    sub2.on_scan_job_failed.assert_not_called()


def test_on_periodic_job_failure(jira_lib_mock):

    sub1 = Mock()
    sub2 = Mock()

    fake_bazel = Mock()
    fake_bazel.get_findings.side_effect = OSError("Call failed")

    scanner_job = BazelICScanner(fake_bazel, jira_lib_mock, [sub1, sub2])

    scanner_job.on_periodic_scan()
    sub1.on_scan_job_succeeded.assert_not_called()
    sub2.on_scan_job_succeeded.assert_not_called()
    sub1.on_scan_job_failed.assert_called_once()
    sub2.on_scan_job_failed.assert_called_once()


def test_on_merge_request_no_changes_to_dependency_files(jira_lib_mock):
    sub1 = Mock()
    sub2 = Mock()
    fake_bazel = Mock()
    fake_bazel.has_dependencies_changed.return_value = {"external_crates_bzl": False, "cargo_lock_toml": False}
    scanner_job = BazelICScanner(fake_bazel, jira_lib_mock, [sub1, sub2])

    scanner_job.on_merge_request_scan()
    fake_bazel.get_modified_packages.assert_not_called()
    fake_bazel.get_dependency_diff.assert_not_called()
    fake_bazel.get_findings.assert_not_called()
    sub1.on_scan_job_succeeded.assert_called_once()
    sub2.on_scan_job_succeeded.assert_called_once()
    sub1.on_scan_job_failed.assert_not_called()
    sub2.on_scan_job_failed.assert_not_called()


def test_on_merge_request_changes_to_dependency_files_but_not_external_crates(jira_lib_mock):
    sub1 = Mock()
    sub2 = Mock()
    fake_bazel = Mock()
    fake_bazel.has_dependencies_changed.return_value = {"external_crates_bzl": False, "cargo_lock_toml": True}
    scanner_job = BazelICScanner(fake_bazel, jira_lib_mock, [sub1, sub2])

    scanner_job.on_merge_request_scan()
    fake_bazel.get_modified_packages.assert_called_once()
    fake_bazel.get_dependency_diff.assert_called_once()
    fake_bazel.get_findings.assert_not_called()
    sub1.on_scan_job_succeeded.assert_called_once()
    sub2.on_scan_job_succeeded.assert_called_once()
    sub1.on_scan_job_failed.assert_not_called()
    sub2.on_scan_job_failed.assert_not_called()


def test_on_merge_request_changes_no_findings(jira_lib_mock):
    sub1 = Mock()
    sub2 = Mock()
    fake_bazel = Mock()
    fake_bazel.has_dependencies_changed.return_value = {"external_crates_bzl": True, "cargo_lock_toml": True}
    fake_bazel.get_modified_packages.return_value = ["package1", "package2"]
    fake_bazel.get_findings.return_value = []
    scanner_job = BazelICScanner(fake_bazel, jira_lib_mock, [sub1, sub2])

    scanner_job.on_merge_request_scan()
    fake_bazel.get_modified_packages.assert_called_once()
    fake_bazel.get_dependency_diff.assert_called_once()
    fake_bazel.get_findings.assert_called_once()
    jira_lib_mock.get_open_finding.assert_not_called()
    sub1.on_scan_job_succeeded.assert_called_once()
    sub2.on_scan_job_succeeded.assert_called_once()
    sub1.on_scan_job_failed.assert_not_called()
    sub2.on_scan_job_failed.assert_not_called()


def on_merge_request_changes_all_findings_have_jira_findings(jira_lib_mock):
    scanner = "BAZEL_IC"
    repository = "ic"
    sub1 = Mock()
    sub2 = Mock()
    fake_bazel = Mock()
    fake_bazel.has_dependencies_changed.return_value = {"external_crates_bzl": True, "cargo_lock_toml": True}
    fake_bazel.get_modified_packages.return_value = ["package1", "package2"]
    fake_bazel.get_findings.return_value = [
        Finding(
            repository=repository,
            scanner=scanner,
            vulnerable_dependency=Dependency("VDID1", "chrono", "1.0", {"VID1": ["1.1", "2.0"]}),
            vulnerabilities=[Vulnerability("VID1", "CVE-123", "huuughe vuln", 100)],
            first_level_dependencies=[Dependency("VDID2", "fl dep", "0.1 beta", {"VID1": ["3.0 alpha"]})],
            projects=["foo", "bar", "bear"],
            risk_assessor=[],
            score=100,
        )
    ]

    jira_finding = JiraFinding(
        repository=repository,
        scanner=scanner,
        vulnerable_dependency=Dependency("VDID1", "chrono", "1.0", {"VID1": ["1.1", "2.0"]}),
        vulnerabilities=[Vulnerability("VID1", "CVE-123", "huuughe vuln", 100)],
        first_level_dependencies=[Dependency("VDID2", "fl dep", "0.1 beta", {"VID1": ["3.0 alpha"]})],
        projects=["foo", "bar", "bear"],
        risk_assessor=[User("mickey", "Mickey Mouse")],
        risk=SecurityRisk.INFORMATIONAL,
        patch_responsible=[],
        due_date=100,
        score=100,
    )
    jira_lib_mock.get_open_finding.return_value = jira_finding

    scanner_job = BazelICScanner(fake_bazel, jira_lib_mock, [sub1, sub2])

    scanner_job.on_merge_request_scan()
    fake_bazel.get_modified_packages.assert_called_once()
    fake_bazel.get_dependency_diff.assert_called_once()
    fake_bazel.get_findings.assert_called_once()
    jira_lib_mock.get_open_finding.assert_called_once()
    jira_lib_mock.commit_has_block_exception.assert_not_called()
    sub1.on_scan_job_succeeded.assert_called_once()
    sub2.on_scan_job_succeeded.assert_called_once()
    sub1.on_scan_job_failed.assert_not_called()
    sub2.on_scan_job_failed.assert_not_called()


@patch("scanner.gitlab_comment.GitlabComment.comment_on_gitlab")
def test_on_merge_request_changes_with_findings_to_flag_and_commit_exception(gitlab_comment_mock, jira_lib_mock):
    scanner = "BAZEL_IC"
    repository = "ic"
    sub1 = Mock()
    sub2 = Mock()
    fake_bazel = Mock()
    fake_bazel.has_dependencies_changed.return_value = {"external_crates_bzl": True, "cargo_lock_toml": True}

    fake_bazel.get_modified_packages.return_value = ["package1", "package2"]
    fake_bazel.get_findings.return_value = [
        Finding(
            repository=repository,
            scanner=scanner,
            vulnerable_dependency=Dependency("VDID1", "chrono", "1.0", {"VID1": ["1.1", "2.0"]}),
            vulnerabilities=[Vulnerability("VID1", "CVE-123", "huuughe vuln", 100)],
            first_level_dependencies=[Dependency("VDID2", "fl dep", "0.1 beta", {"VID1": ["3.0 alpha"]})],
            projects=["foo", "bar", "bear"],
            risk_assessor=[],
            score=100,
        )
    ]
    jira_lib_mock.get_open_finding.return_value = []
    jira_lib_mock.commit_has_block_exception.return_value = "commit string"
    scanner_job = BazelICScanner(fake_bazel, jira_lib_mock, [sub1, sub2])

    scanner_job.on_merge_request_scan()
    fake_bazel.get_modified_packages.assert_called_once()
    fake_bazel.get_dependency_diff.assert_called_once()
    fake_bazel.get_findings.assert_called_once()
    jira_lib_mock.get_open_finding.assert_called_once()
    jira_lib_mock.commit_has_block_exception.assert_called_once()
    gitlab_comment_mock.assert_not_called()
    sub1.on_scan_job_succeeded.assert_called_once()
    sub2.on_scan_job_succeeded.assert_called_once()
    sub1.on_scan_job_failed.assert_not_called()
    sub2.on_scan_job_failed.assert_not_called()


@patch("scanner.gitlab_comment.GitlabComment.comment_on_gitlab")
def test_on_merge_request_changes_with_findings_to_flag_no_commit_exception(gitlab_comment_mock, jira_lib_mock):
    scanner = "BAZEL_IC"
    repository = "ic"
    sub1 = Mock()
    sub2 = Mock()
    fake_bazel = Mock()
    fake_bazel.has_dependencies_changed.return_value = {"external_crates_bzl": True, "cargo_lock_toml": True}

    fake_bazel.get_modified_packages.return_value = ["foo"]
    fake_bazel.get_dependency_diff.return_value = [Dependency("VDID3", "fl dep", "0.1 beta", {"VID1": ["3.0 alpha"]})]
    fake_bazel.get_findings.return_value = [
        Finding(
            repository=repository,
            scanner=scanner,
            vulnerable_dependency=Dependency("VDID1", "chrono", "1.0", {"VID1": ["1.1", "2.0"]}),
            vulnerabilities=[Vulnerability("VID1", "CVE-123", "huuughe vuln", 100)],
            first_level_dependencies=[
                Dependency("VDID2", "fl dep", "0.1 beta", {"VID1": ["3.0 alpha"]}),
                Dependency("VDID3", "fl dep", "0.1 beta", {"VID1": ["3.0 alpha"]}),
            ],
            projects=["foo", "bar", "bear"],
            risk_assessor=[],
            score=100,
        )
    ]
    jira_lib_mock.get_open_finding.return_value = []
    jira_lib_mock.commit_has_block_exception.return_value = None
    scanner_job = BazelICScanner(fake_bazel, jira_lib_mock, [sub1, sub2])

    with pytest.raises(SystemExit) as e:
        scanner_job.on_merge_request_scan()
    assert e.type == SystemExit
    assert e.value.code == 1

    fake_bazel.get_modified_packages.assert_called_once()
    fake_bazel.get_dependency_diff.assert_called_once()
    fake_bazel.get_findings.assert_called_once()
    jira_lib_mock.get_open_finding.assert_called_once()
    jira_lib_mock.commit_has_block_exception.assert_called_once()
    finding_to_flag = [
        Finding(
            repository=repository,
            scanner=scanner,
            vulnerable_dependency=Dependency("VDID1", "chrono", "1.0", {"VID1": ["1.1", "2.0"]}),
            vulnerabilities=[Vulnerability("VID1", "CVE-123", "huuughe vuln", 100)],
            first_level_dependencies=[Dependency("VDID3", "fl dep", "0.1 beta", {"VID1": ["3.0 alpha"]})],
            projects=["foo"],
            risk_assessor=[],
            score=100,
        )
    ]
    gitlab_comment_mock.assert_called_once_with(info=finding_to_flag)
    sub1.on_merge_request_blocked.assert_called_once()
    sub2.on_merge_request_blocked.assert_called_once()
    sub1.on_scan_job_succeeded.assert_called_once()
    sub2.on_scan_job_succeeded.assert_called_once()
    sub1.on_scan_job_failed.assert_not_called()
    sub2.on_scan_job_failed.assert_not_called()


def test_on_merge_request_job_failed(jira_lib_mock):
    sub1 = Mock()
    sub2 = Mock()

    fake_bazel = Mock()
    fake_bazel.has_dependencies_changed.side_effect = OSError("Call failed")

    scanner_job = BazelICScanner(fake_bazel, jira_lib_mock, [sub1, sub2])
    scanner_job.on_merge_request_scan()
    sub1.on_scan_job_failed.assert_called_once()
    sub2.on_scan_job_failed.assert_called_once()
    sub1.on_scan_job_succeeded.assert_not_called()
    sub2.on_scan_job_succeeded.assert_not_called()


def test_on_release_scan_no_findings(jira_lib_mock):
    sub1 = Mock()
    sub2 = Mock()
    fake_bazel = Mock()
    fake_bazel.get_findings.return_value = []
    scanner_job = BazelICScanner(fake_bazel, jira_lib_mock, [sub1, sub2])

    scanner_job.on_release_scan()
    sub1.on_scan_job_succeeded.assert_called_once()
    sub2.on_scan_job_succeeded.assert_called_once()
    sub1.on_scan_job_failed.assert_not_called()
    sub2.on_scan_job_failed.assert_not_called()


def test_on_release_scan_findings_have_jira_findings_with_no_risk(jira_lib_mock):
    scanner = "BAZEL_IC"
    repository = "ic"

    sub1 = Mock()
    sub2 = Mock()
    fake_bazel = Mock()

    fake_bazel.get_findings.return_value = [
        Finding(
            repository=repository,
            scanner=scanner,
            vulnerable_dependency=Dependency("VDID1", "chrono", "1.0", {"VID1": ["1.1", "2.0"]}),
            vulnerabilities=[Vulnerability("VID1", "CVE-123", "huuughe vuln", 100)],
            first_level_dependencies=[Dependency("VDID2", "fl dep", "0.1 beta", {"VID1": ["3.0 alpha"]})],
            projects=["foo", "bar", "bear"],
            risk_assessor=[],
            score=100,
        )
    ]

    jira_finding = JiraFinding(
        repository=repository,
        scanner=scanner,
        vulnerable_dependency=Dependency("VDID1", "chrono", "1.0", {"VID1": ["1.1", "2.0"]}),
        vulnerabilities=[Vulnerability("VID1", "CVE-123", "huuughe vuln", 100)],
        first_level_dependencies=[Dependency("VDID2", "fl dep", "0.1 beta", {"VID1": ["3.0 alpha"]})],
        projects=["foo", "bar", "bear"],
        risk_assessor=[User("mickey", "Mickey Mouse")],
        risk=None,
        patch_responsible=[],
        due_date=100,
        score=100,
    )
    jira_lib_mock.get_open_finding.return_value = jira_finding
    jira_lib_mock.commit_has_block_exception.return_value = None

    scanner_job = BazelICScanner(fake_bazel, jira_lib_mock, [sub1, sub2])

    scanner_job.on_release_scan()
    fake_bazel.get_findings.assert_called_once()
    jira_lib_mock.get_open_finding.assert_called_once()
    jira_lib_mock.commit_has_block_exception.assert_called_once()
    sub1.on_release_build_blocked.assert_called_once()
    sub2.on_release_build_blocked.assert_called_once()
    sub1.on_scan_job_succeeded.assert_called_once()
    sub2.on_scan_job_succeeded.assert_called_once()
    sub1.on_scan_job_failed.assert_not_called()
    sub2.on_scan_job_failed.assert_not_called()


def test_on_release_scan_findings_have_jira_findings_with_no_risk_with_exception(jira_lib_mock):
    scanner = "BAZEL_IC"
    repository = "ic"

    sub1 = Mock()
    sub2 = Mock()
    fake_bazel = Mock()

    fake_bazel.get_findings.return_value = [
        Finding(
            repository=repository,
            scanner=scanner,
            vulnerable_dependency=Dependency("VDID1", "chrono", "1.0", {"VID1": ["1.1", "2.0"]}),
            vulnerabilities=[Vulnerability("VID1", "CVE-123", "huuughe vuln", 100)],
            first_level_dependencies=[Dependency("VDID2", "fl dep", "0.1 beta", {"VID1": ["3.0 alpha"]})],
            projects=["foo", "bar", "bear"],
            risk_assessor=[],
            score=100,
        )
    ]

    jira_finding = JiraFinding(
        repository=repository,
        scanner=scanner,
        vulnerable_dependency=Dependency("VDID1", "chrono", "1.0", {"VID1": ["1.1", "2.0"]}),
        vulnerabilities=[Vulnerability("VID1", "CVE-123", "huuughe vuln", 100)],
        first_level_dependencies=[Dependency("VDID2", "fl dep", "0.1 beta", {"VID1": ["3.0 alpha"]})],
        projects=["foo", "bar", "bear"],
        risk_assessor=[User("mickey", "Mickey Mouse")],
        risk=None,
        patch_responsible=[],
        due_date=100,
        score=100,
    )
    jira_lib_mock.get_open_finding.return_value = jira_finding
    jira_lib_mock.commit_has_block_exception.return_value = "test commit"

    scanner_job = BazelICScanner(fake_bazel, jira_lib_mock, [sub1, sub2])

    scanner_job.on_release_scan()
    fake_bazel.get_findings.assert_called_once()
    jira_lib_mock.get_open_finding.assert_called_once()
    jira_lib_mock.commit_has_block_exception.assert_called_once()
    sub1.on_release_build_blocked.assert_not_called()
    sub2.on_release_build_blocked.assert_not_called()
    sub1.on_scan_job_succeeded.assert_called_once()
    sub2.on_scan_job_succeeded.assert_called_once()
    sub1.on_scan_job_failed.assert_not_called()
    sub2.on_scan_job_failed.assert_not_called()


def test_on_release_scan_findings_have_jira_findings_with_high_risk_but_no_due_date(jira_lib_mock):
    scanner = "BAZEL_IC"
    repository = "ic"

    sub1 = Mock()
    sub2 = Mock()
    fake_bazel = Mock()

    fake_bazel.get_findings.return_value = [
        Finding(
            repository=repository,
            scanner=scanner,
            vulnerable_dependency=Dependency("VDID1", "chrono", "1.0", {"VID1": ["1.1", "2.0"]}),
            vulnerabilities=[Vulnerability("VID1", "CVE-123", "huuughe vuln", 100)],
            first_level_dependencies=[Dependency("VDID2", "fl dep", "0.1 beta", {"VID1": ["3.0 alpha"]})],
            projects=["foo", "bar", "bear"],
            risk_assessor=[],
            score=100,
        )
    ]

    jira_finding = JiraFinding(
        repository=repository,
        scanner=scanner,
        vulnerable_dependency=Dependency("VDID1", "chrono", "1.0", {"VID1": ["1.1", "2.0"]}),
        vulnerabilities=[Vulnerability("VID1", "CVE-123", "huuughe vuln", 100)],
        first_level_dependencies=[Dependency("VDID2", "fl dep", "0.1 beta", {"VID1": ["3.0 alpha"]})],
        projects=["foo", "bar", "bear"],
        risk_assessor=[User("mickey", "Mickey Mouse")],
        risk=SecurityRisk.CRITICAL,
        patch_responsible=[],
        due_date=None,
        score=100,
    )
    jira_lib_mock.get_open_finding.return_value = jira_finding
    jira_lib_mock.commit_has_block_exception.return_value = None

    scanner_job = BazelICScanner(fake_bazel, jira_lib_mock, [sub1, sub2])

    scanner_job.on_release_scan()
    fake_bazel.get_findings.assert_called_once()
    jira_lib_mock.get_open_finding.assert_called_once()
    jira_lib_mock.commit_has_block_exception.assert_called_once()
    sub1.on_release_build_blocked.assert_called_once()
    sub2.on_release_build_blocked.assert_called_once()
    sub1.on_scan_job_succeeded.assert_called_once()
    sub2.on_scan_job_succeeded.assert_called_once()
    sub1.on_scan_job_failed.assert_not_called()
    sub2.on_scan_job_failed.assert_not_called()


def test_on_release_scan_findings_have_jira_findings_with_high_risk_but_valid_due_date(jira_lib_mock):
    scanner = "BAZEL_IC"
    repository = "ic"

    sub1 = Mock()
    sub2 = Mock()
    fake_bazel = Mock()

    fake_bazel.get_findings.return_value = [
        Finding(
            repository=repository,
            scanner=scanner,
            vulnerable_dependency=Dependency("VDID1", "chrono", "1.0", {"VID1": ["1.1", "2.0"]}),
            vulnerabilities=[Vulnerability("VID1", "CVE-123", "huuughe vuln", 100)],
            first_level_dependencies=[Dependency("VDID2", "fl dep", "0.1 beta", {"VID1": ["3.0 alpha"]})],
            projects=["foo", "bar", "bear"],
            risk_assessor=[],
            score=100,
        )
    ]

    jira_finding = JiraFinding(
        repository=repository,
        scanner=scanner,
        vulnerable_dependency=Dependency("VDID1", "chrono", "1.0", {"VID1": ["1.1", "2.0"]}),
        vulnerabilities=[Vulnerability("VID1", "CVE-123", "huuughe vuln", 100)],
        first_level_dependencies=[Dependency("VDID2", "fl dep", "0.1 beta", {"VID1": ["3.0 alpha"]})],
        projects=["foo", "bar", "bear"],
        risk_assessor=[User("mickey", "Mickey Mouse")],
        risk=SecurityRisk.CRITICAL,
        patch_responsible=[],
        due_date=int(datetime.datetime.utcnow().timestamp()) + 10000,
        score=100,
    )
    jira_lib_mock.get_open_finding.return_value = jira_finding

    scanner_job = BazelICScanner(fake_bazel, jira_lib_mock, [sub1, sub2])

    scanner_job.on_release_scan()
    fake_bazel.get_findings.assert_called_once()
    jira_lib_mock.get_open_finding.assert_called_once()
    sub1.on_scan_job_succeeded.assert_called_once()
    sub2.on_scan_job_succeeded.assert_called_once()
    sub1.on_scan_job_failed.assert_not_called()
    sub2.on_scan_job_failed.assert_not_called()


def test_on_release_scan_findings_have_jira_findings_with_high_risk_but_expired_due_date(jira_lib_mock):
    scanner = "BAZEL_IC"
    repository = "ic"

    sub1 = Mock()
    sub2 = Mock()
    fake_bazel = Mock()

    fake_bazel.get_findings.return_value = [
        Finding(
            repository=repository,
            scanner=scanner,
            vulnerable_dependency=Dependency("VDID1", "chrono", "1.0", {"VID1": ["1.1", "2.0"]}),
            vulnerabilities=[Vulnerability("VID1", "CVE-123", "huuughe vuln", 100)],
            first_level_dependencies=[Dependency("VDID2", "fl dep", "0.1 beta", {"VID1": ["3.0 alpha"]})],
            projects=["foo", "bar", "bear"],
            risk_assessor=[],
            score=100,
        )
    ]

    jira_finding = JiraFinding(
        repository=repository,
        scanner=scanner,
        vulnerable_dependency=Dependency("VDID1", "chrono", "1.0", {"VID1": ["1.1", "2.0"]}),
        vulnerabilities=[Vulnerability("VID1", "CVE-123", "huuughe vuln", 100)],
        first_level_dependencies=[Dependency("VDID2", "fl dep", "0.1 beta", {"VID1": ["3.0 alpha"]})],
        projects=["foo", "bar", "bear"],
        risk_assessor=[User("mickey", "Mickey Mouse")],
        risk=SecurityRisk.CRITICAL,
        patch_responsible=[],
        due_date=100,
        score=100,
    )
    jira_lib_mock.get_open_finding.return_value = jira_finding
    jira_lib_mock.commit_has_block_exception.return_value = None

    scanner_job = BazelICScanner(fake_bazel, jira_lib_mock, [sub1, sub2])

    scanner_job.on_release_scan()
    fake_bazel.get_findings.assert_called_once()
    jira_lib_mock.get_open_finding.assert_called_once()
    jira_lib_mock.commit_has_block_exception.assert_called_once()
    sub1.on_release_build_blocked.assert_called_once()
    sub2.on_release_build_blocked.assert_called_once()
    sub1.on_scan_job_succeeded.assert_called_once()
    sub2.on_scan_job_succeeded.assert_called_once()
    sub1.on_scan_job_failed.assert_not_called()
    sub2.on_scan_job_failed.assert_not_called()


def test_on_release_scan_findings_have_jira_findings_with_high_risk_but_expired_due_date_with_exception(jira_lib_mock):
    scanner = "BAZEL_IC"
    repository = "ic"

    sub1 = Mock()
    sub2 = Mock()
    fake_bazel = Mock()

    fake_bazel.get_findings.return_value = [
        Finding(
            repository=repository,
            scanner=scanner,
            vulnerable_dependency=Dependency("VDID1", "chrono", "1.0", {"VID1": ["1.1", "2.0"]}),
            vulnerabilities=[Vulnerability("VID1", "CVE-123", "huuughe vuln", 100)],
            first_level_dependencies=[Dependency("VDID2", "fl dep", "0.1 beta", {"VID1": ["3.0 alpha"]})],
            projects=["foo", "bar", "bear"],
            risk_assessor=[],
            score=100,
        )
    ]

    jira_finding = JiraFinding(
        repository=repository,
        scanner=scanner,
        vulnerable_dependency=Dependency("VDID1", "chrono", "1.0", {"VID1": ["1.1", "2.0"]}),
        vulnerabilities=[Vulnerability("VID1", "CVE-123", "huuughe vuln", 100)],
        first_level_dependencies=[Dependency("VDID2", "fl dep", "0.1 beta", {"VID1": ["3.0 alpha"]})],
        projects=["foo", "bar", "bear"],
        risk_assessor=[User("mickey", "Mickey Mouse")],
        risk=SecurityRisk.CRITICAL,
        patch_responsible=[],
        due_date=100,
        score=100,
    )
    jira_lib_mock.get_open_finding.return_value = jira_finding
    jira_lib_mock.commit_has_block_exception.return_value = "test commit"

    scanner_job = BazelICScanner(fake_bazel, jira_lib_mock, [sub1, sub2])

    scanner_job.on_release_scan()
    fake_bazel.get_findings.assert_called_once()
    jira_lib_mock.get_open_finding.assert_called_once()
    jira_lib_mock.commit_has_block_exception.assert_called_once()
    sub1.on_release_build_blocked.assert_not_called()
    sub2.on_release_build_blocked.assert_not_called()
    sub1.on_scan_job_succeeded.assert_called_once()
    sub2.on_scan_job_succeeded.assert_called_once()
    sub1.on_scan_job_failed.assert_not_called()
    sub2.on_scan_job_failed.assert_not_called()


def test_on_release_scan_new_findings(jira_lib_mock):
    scanner = "BAZEL_IC"
    repository = "ic"

    sub1 = Mock()
    sub2 = Mock()
    fake_bazel = Mock()

    fake_bazel.get_findings.return_value = [
        Finding(
            repository=repository,
            scanner=scanner,
            vulnerable_dependency=Dependency("VDID1", "chrono", "1.0", {"VID1": ["1.1", "2.0"]}),
            vulnerabilities=[Vulnerability("VID1", "CVE-123", "huuughe vuln", 100)],
            first_level_dependencies=[Dependency("VDID2", "fl dep", "0.1 beta", {"VID1": ["3.0 alpha"]})],
            projects=["foo", "bar", "bear"],
            risk_assessor=[],
            score=100,
        )
    ]
    jira_lib_mock.get_open_finding.return_value = []
    jira_lib_mock.commit_has_block_exception.return_value = None

    scanner_job = BazelICScanner(fake_bazel, jira_lib_mock, [sub1, sub2])

    scanner_job.on_release_scan()
    fake_bazel.get_findings.assert_called_once()
    jira_lib_mock.get_open_finding.assert_called_once()
    jira_lib_mock.commit_has_block_exception.assert_called_once()
    sub1.on_release_build_blocked.assert_called_once()
    sub2.on_release_build_blocked.assert_called_once()
    sub1.on_scan_job_succeeded.assert_called_once()
    sub2.on_scan_job_succeeded.assert_called_once()
    sub1.on_scan_job_failed.assert_not_called()
    sub2.on_scan_job_failed.assert_not_called()


def test_on_release_scan_new_findings_with_exception(jira_lib_mock):
    scanner = "BAZEL_IC"
    repository = "ic"

    sub1 = Mock()
    sub2 = Mock()
    fake_bazel = Mock()

    fake_bazel.get_findings.return_value = [
        Finding(
            repository=repository,
            scanner=scanner,
            vulnerable_dependency=Dependency("VDID1", "chrono", "1.0", {"VID1": ["1.1", "2.0"]}),
            vulnerabilities=[Vulnerability("VID1", "CVE-123", "huuughe vuln", 100)],
            first_level_dependencies=[Dependency("VDID2", "fl dep", "0.1 beta", {"VID1": ["3.0 alpha"]})],
            projects=["foo", "bar", "bear"],
            risk_assessor=[],
            score=100,
        )
    ]
    jira_lib_mock.get_open_finding.return_value = []
    jira_lib_mock.commit_has_block_exception.return_value = "test commit"

    scanner_job = BazelICScanner(fake_bazel, jira_lib_mock, [sub1, sub2])

    scanner_job.on_release_scan()
    fake_bazel.get_findings.assert_called_once()
    jira_lib_mock.get_open_finding.assert_called_once()
    jira_lib_mock.commit_has_block_exception.assert_called_once()
    sub1.on_release_build_blocked.assert_not_called()
    sub2.on_release_build_blocked.assert_not_called()
    sub1.on_scan_job_succeeded.assert_called_once()
    sub2.on_scan_job_succeeded.assert_called_once()
    sub1.on_scan_job_failed.assert_not_called()
    sub2.on_scan_job_failed.assert_not_called()


def test_on_release_scan_job_failed(jira_lib_mock):
    sub1 = Mock()
    sub2 = Mock()
    fake_bazel = Mock()
    fake_bazel.get_findings.side_effect = OSError("Call failed")
    scanner_job = BazelICScanner(fake_bazel, jira_lib_mock, [sub1, sub2])

    scanner_job.on_release_scan()
    sub1.on_scan_job_succeeded.assert_not_called()
    sub2.on_scan_job_succeeded.assert_not_called()
    sub1.on_scan_job_failed.assert_called_once()
    sub2.on_scan_job_failed.assert_called_once()
