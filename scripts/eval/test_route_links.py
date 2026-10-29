"""route_links.py's site search: the request APIs eval/route-links-v1/README.md names, found in a throwaway repo."""
from pathlib import Path
import subprocess
import tempfile
import unittest

import route_links


def repo(files):
    d = Path(tempfile.mkdtemp())
    subprocess.run(['git', 'init', '-q', str(d)], check=True)
    for rel, text in files.items():
        (d/rel).parent.mkdir(parents=True, exist_ok=True)
        (d/rel).write_text(text)
    subprocess.run(['git', '-C', str(d), 'add', '-A'], check=True)
    return d


class GoSites(unittest.TestCase):
    def test_net_http_requests_and_client_do(self):
        d = repo({
            'client/api.go': 'package client\n\n'
                             'func (p *Client) Get(id string) {\n'
                             '\treq, _ := http.NewRequest(http.MethodGet, fmt.Sprintf("%s/api/v2/users/%s", p.HostURL, id), nil)\n'
                             '\tp.HTTPClient.Do(req)\n'
                             '\thttp.Get(base + "/health")\n'
                             '\thttp.DefaultClient.Do(req)\n'
                             '\t// http.Post(base + "/old", "", nil)\n'
                             '\tw.Do(job)\n'
                             '}\n',
            'client/api_test.go': 'package client\n\nfunc TestX() { http.Get("/x") }\n',
            'vendor/x/y.go': 'package y\n\nfunc f() { http.Get("/v") }\n',
        })
        got = [(s['line'], s['api']) for s in route_links.sites(d, 'orchard')]
        # Test files, vendored code, comments and another type's `Do` are not sites.
        self.assertEqual(got, [(4, 'http.NewRequest('), (5, 'HTTPClient.Do('), (6, 'http.Get('), (7, 'http.DefaultClient.Do(')])


if __name__ == '__main__':
    unittest.main()
