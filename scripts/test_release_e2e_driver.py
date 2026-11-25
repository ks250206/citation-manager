"""Unit tests for the release E2E driver's step bookkeeping (tests/e2e/release/run.py)."""
import importlib.util
from pathlib import Path
import types
import unittest

ROOT = Path(__file__).resolve().parents[1]
spec = importlib.util.spec_from_file_location('release_e2e_run', ROOT / 'tests/e2e/release/run.py')
run = importlib.util.module_from_spec(spec)
spec.loader.exec_module(run)


def driver(in_netns=True):
    instance = object.__new__(run.Run)
    instance.args = types.SimpleNamespace(in_netns=in_netns)
    instance.steps, instance.checks, instance.results = [], {}, {}
    return instance


class StepBookkeeping(unittest.TestCase):
    def test_a_skipped_step_adds_no_network_check(self):
        d = driver()
        d.step('gui_restart', lambda: None, skip='needs --gui')
        d.no_external_traffic('gui_restart')
        self.assertEqual(d.checks, {})  # previously a failing `.no_external_network_attempt`

    def test_a_step_that_ran_is_still_checked(self):
        d = driver()
        d.steps.append(dict(name='seed', status='completed',
                            network=dict(ipv4_no_route=0, ipv6_no_route=0)))
        d.no_external_traffic('seed')
        self.assertEqual(d.checks, {'seed.no_external_network_attempt': True})
        d.steps.append(dict(name='leaky', status='completed',
                            network=dict(ipv4_no_route=2, ipv6_no_route=0)))
        d.no_external_traffic('leaky')
        self.assertFalse(d.checks['leaky.no_external_network_attempt'])

    def test_step_ran_ignores_skipped_steps(self):
        d = driver()
        d.step('annotate_and_export', lambda: None, skip='no --owner-client')
        self.assertFalse(d.step_ran('annotate_and_export'))
        d.steps.append(dict(name='annotate_and_export', status='failed'))
        self.assertTrue(d.step_ran('annotate_and_export'))


if __name__ == '__main__':
    unittest.main()
