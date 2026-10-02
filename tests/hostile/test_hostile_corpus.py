"""Gate hook for the hostile corpus self test (AC-04, QA-1).

Runs `tests/hostile/run.sh --self-test`, which needs no compiler: it builds the seeded
generator, checks that every required case exists, that `expect.txt` only names existing
cases and that the generator is deterministic. The full compiler run is started by the
gate check `45_hostile.sh` once the CLI exists.
"""

import pathlib
import subprocess
import unittest

HERE = pathlib.Path(__file__).resolve().parent


class HostileCorpusSelfTest(unittest.TestCase):
    def test_ac_04_hostile_corpus_self_test(self):
        result = subprocess.run(
            ["bash", str(HERE / "run.sh"), "--self-test"],
            capture_output=True,
            text=True,
            timeout=300,
        )
        self.assertEqual(result.returncode, 0, result.stdout + result.stderr)
        self.assertIn("HOSTILE SELF-TEST: OK", result.stdout)


if __name__ == "__main__":
    unittest.main()
