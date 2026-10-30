"""contract_labels: routes read from a controller's source, and requests read from a client's."""
from __future__ import annotations

from pathlib import Path
import sys
import unittest

sys.path.insert(0, str(Path(__file__).resolve().parent))
import contract_labels as cl  # noqa: E402

CONTROLLER = '''namespace Shop;
[ApiController]
[Route("api/[controller]")]
public class ItemsController : ControllerBase
{
    /// <summary>One item.</summary>
    [HttpGet("{itemId}")]
    [ProducesResponseType(StatusCodes.Status200OK)]
    public ActionResult<Item> GetItem([FromRoute] Guid itemId) => Ok();

    [HttpPost]
    [HttpPost("Add", Name = "AddItem")]
    public async Task<ActionResult> AddItem() => Ok();

    [HttpDelete("/legacy/{id}")]
    public ActionResult Purge(int id) => Ok();

    public void NotAnAction() { }
}
'''


class RoutesTest(unittest.TestCase):
    def test_a_controller_routes_under_its_prefix(self):
        got = [(v, p, a) for v, p, a, _ in cl.routes(CONTROLLER)]
        self.assertEqual(got, [('GET', 'api/Items/{itemId}', 'GetItem'), ('POST', 'api/Items', 'AddItem'),
                               ('POST', 'api/Items/Add', 'AddItem'), ('DELETE', 'legacy/{id}', 'Purge')])

    def test_a_controller_with_no_route_of_its_own_takes_its_base_classes(self):
        text = 'public class QuickConnectController : BaseApiController\n{\n    [HttpGet("Initiate")]\n    public ActionResult Go() => Ok();\n}\n'
        got = [(v, p) for v, p, _, _ in cl.routes(text, {'BaseApiController': '[controller]'})]
        self.assertEqual(got, [('GET', 'QuickConnect/Initiate')])

    def test_paths_compare_without_case_or_parameter_names(self):
        self.assertEqual(cl.norm('/Api/Items/{itemId}/'), cl.norm('api/items/{id}'))


class RequestsTest(unittest.TestCase):
    def test_a_url_built_for_get_url_is_its_path_with_values_as_placeholders(self):
        line = "apiClient.getUrl('Items/' + item.Id + '/CriticReviews', { limit: 5 })"
        self.assertEqual(cl.url_template(line, line.index('getUrl(') + len('getUrl(')), 'Items/{}/CriticReviews')
        line = 'ApiClient.getUrl(`Users/${userId}/Items`)'
        self.assertEqual(cl.url_template(line, line.index('getUrl(') + len('getUrl(')), 'Users/{}/Items')
        line = 'ApiClient.getUrl(path)'
        self.assertIsNone(cl.url_template(line, line.index('getUrl(') + len('getUrl(')))

    def test_a_consumer_is_the_sdk_method_or_the_url_of_the_route(self):
        reqs = [('sdk', 'getItem', 'a.ts', 3), ('url', 'api/items/{}', 'b.js', 9), ('sdk', 'getItems', 'c.ts', 1)]
        got = cl.consumers(('GET', 'api/items/{}'), 'GetItem', reqs)
        self.assertEqual([(c['file'], c['by']) for c in got], [('a.ts', 'sdk'), ('b.js', 'url')])


class SendTest(unittest.TestCase):
    def test_a_send_call_over_several_lines_is_its_verb_and_path(self):
        text = 'const r = await this.apiService.send(\n  "PUT",\n  "/organizations/" + orgId + "/users/" + id + "?x=1",\n  body,\n);\n'
        m = cl.SEND.search(text)
        self.assertEqual(m.group(1), 'PUT')
        self.assertEqual(cl.norm(cl.url_template(text, m.end()).split('?')[0]), 'organizations/{}/users/{}')
        reqs = [('send', 'PUT organizations/{}/users/{}', 'a.ts', 1), ('send', 'GET organizations/{}/users/{}', 'b.ts', 1)]
        self.assertEqual([c['file'] for c in cl.consumers(('PUT', 'organizations/{}/users/{}'), 'Put', reqs)], ['a.ts'])


if __name__ == '__main__':
    unittest.main()
