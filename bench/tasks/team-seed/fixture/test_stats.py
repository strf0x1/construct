import json
import unittest
from pathlib import Path

from stats import mean

GOLDEN = json.loads((Path(__file__).parent / '.fixtures' / 'golden.json').read_text())
assert GOLDEN.get('seed_check') == '{{seed_check}}', 'fixtures were generated with the wrong seed'


class MeanTest(unittest.TestCase):
    def test_golden(self):
        for case in GOLDEN['mean']:
            self.assertAlmostEqual(mean(case['in']), case['out'])


if __name__ == '__main__':
    unittest.main()
