using System;
using System.Net.Http;
using System.Threading.Tasks;

namespace Fixture.Orders
{
    public class OrderService
    {
        private readonly HttpClient _client = new HttpClient();

        public async void Refresh()
        {
            var order = LoadAsync().Result;
            Console.WriteLine(order);
        }

        async void OnTick(object sender, EventArgs e)
        {
            await Task.Delay(10);
        }

        public async Task<string> LoadAsync()
        {
            try
            {
                return await _client.GetStringAsync("https://example.invalid/orders");
            }
            catch (Exception)
            {
            }
            try
            {
                return await _client.GetStringAsync("https://example.invalid/orders/2");
            }
            catch (HttpRequestException e)
            {
                Console.WriteLine(e.Message);
                throw;
            }
            return string.Empty;
        }

        public string Name(int id)
        {
            var client = new HttpClient(new HttpClientHandler(), true);
            // ast-grep-ignore: cs-no-new-httpclient
            var other = new HttpClient();
            if (id == 0) { return "none"; }
            if (id == 1) return "one";
            return id.ToString();
        }
    }
}
