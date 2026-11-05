"""identity-v2's Go cross-check against scip-go: reading scip-go's symbols and judging agreement, on made-up rows."""
import importlib.util
from pathlib import Path
import unittest

HERE = Path(__file__).resolve().parent


def load(name, path):
    spec = importlib.util.spec_from_file_location(name, path)
    module = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(module)
    return module


crosscheck = load('identity_go_crosscheck', HERE/'identity/go/crosscheck.py')


def ref(**kw):
    row = dict(t='ref', repo='shop', file='cart/cart.go', line=10, col=5, name='Total', inside=True,
               locs=[['cart/cart.go', 3]], origin=None, symbol='example.com/shop/cart.Cart.Total')
    row.update(kw)
    return row


class PackageAndName(unittest.TestCase):
    def test_standard_library(self):
        self.assertEqual(crosscheck.package_and_name('scip-go gomod github.com/golang/go/src go1.15 io/Writer#Write().'),
                         ('io', 'Write'))
        self.assertEqual(crosscheck.package_and_name('scip-go gomod github.com/golang/go/src go1.15 net/http/Header#Get().'),
                         ('net/http', 'Get'))
        self.assertEqual(crosscheck.package_and_name('scip-go gomod github.com/golang/go/src go1.15 io/Discard.'),
                         ('io', 'Discard'))

    def test_quoted_package(self):
        self.assertEqual(crosscheck.package_and_name(
            'scip-go gomod example.com/shop v1.2.0 `example.com/shop/cart`/Cart#items.'), ('example.com/shop/cart', 'items'))

    def test_not_scip_go(self):
        self.assertEqual(crosscheck.package_and_name('local 4'), (None, None))


class Judge(unittest.TestCase):
    defs = {'scip-go gomod example.com/shop . `example.com/shop/cart`/Cart#Total().': ('cart/cart.go', 3),
            'cart/cart_test.go local 7': ('cart/cart_test.go', 12)}

    def test_inside_same_place(self):
        self.assertEqual(crosscheck.judge(ref(), ['scip-go gomod example.com/shop . `example.com/shop/cart`/Cart#Total().'],
                                          self.defs), 'agree')

    def test_inside_local_symbol_at_the_same_place(self):
        row = ref(file='cart/cart_test.go', locs=[['cart/cart_test.go', 12]])
        self.assertEqual(crosscheck.judge(row, ['cart/cart_test.go local 7'], self.defs), 'agree')

    def test_inside_other_place(self):
        row = ref(locs=[['cart/cart.go', 4]])
        self.assertEqual(crosscheck.judge(row, ['scip-go gomod example.com/shop . `example.com/shop/cart`/Cart#Total().'],
                                          self.defs), 'scip-go: other inside target')

    def test_inside_but_scip_go_has_no_definition(self):
        self.assertEqual(crosscheck.judge(ref(), ['scip-go gomod example.com/other v1 `example.com/other`/Cart#Total().'],
                                          self.defs), 'scip-go: outside or local')

    def test_outside_same_package_and_name(self):
        row = ref(name='Get', inside=False, locs=[], origin='stdlib', symbol='net/http.Header.Get')
        self.assertEqual(crosscheck.judge(row, ['scip-go gomod github.com/golang/go/src go1.15 net/http/Header#Get().'],
                                          self.defs), 'agree')

    def test_outside_other_package(self):
        row = ref(name='Get', inside=False, locs=[], origin='stdlib', symbol='net/url.Values.Get')
        self.assertEqual(crosscheck.judge(row, ['scip-go gomod github.com/golang/go/src go1.15 net/http/Header#Get().'],
                                          self.defs), 'scip-go: other outside target')

    def test_outside_but_scip_go_inside(self):
        row = ref(inside=False, locs=[], origin='package')
        self.assertEqual(crosscheck.judge(row, ['scip-go gomod example.com/shop . `example.com/shop/cart`/Cart#Total().'],
                                          self.defs), 'scip-go: inside target')


if __name__ == '__main__':
    unittest.main()
