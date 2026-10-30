"""Tests for find_dev.py: the forms a name is asked in, and what each option keeps of find's rows."""
import unittest

import find_dev


class FormsTest(unittest.TestCase):
    def test_each_form_when_it_differs(self):
        self.assertEqual(find_dev.forms('ParseFlags'), [
            ('exact', 'ParseFlags'), ('lower', 'parseflags'), ('prefix', 'ParseF'), ('typo', 'PraseFlags')])
        # Already lower case, too short to cut, second and third letters equal: only what differs is asked.
        self.assertEqual(find_dev.forms('add'), [('exact', 'add')])
        self.assertEqual(find_dev.forms('book'), [('exact', 'book'), ('prefix', 'boo')])


class OptionsTest(unittest.TestCase):
    def rows(self, *names):
        return [{'file': 'a.go', 'line': i + 1, 'name': n, 'matched': 'x'} for i, n in enumerate(names)]

    def test_an_exact_row_keeps_the_page_to_exact_rows(self):
        rows = self.rows('Parse', 'ParseFlags', 'parse', 'Parse')
        o = find_dev.options(rows, 'Parse')
        self.assertEqual([r['line'] for r in o['current'][0]], [1, 2, 3, 4])
        self.assertEqual([r['line'] for r in o['exact-only'][0]], [1, 4])
        self.assertEqual([r['line'] for r in o['similar-apart'][1]], [2, 3])
        self.assertEqual(o['cursor'], (o['exact-only'][0], []))

    def test_without_an_exact_row_every_option_is_current(self):
        rows = self.rows('ParseFlags', 'parse')
        o = find_dev.options(rows, 'Pars')
        self.assertTrue(all(o[k] == (rows, []) for k in find_dev.OPTIONS))


    def test_a_similar_line_is_read_as_its_place(self):
        self.assertEqual(find_dev.similar_row('method Book.ledger  src/a b.py:12'), {'file': 'src/a b.py', 'line': 12})


if __name__ == '__main__':
    unittest.main()
