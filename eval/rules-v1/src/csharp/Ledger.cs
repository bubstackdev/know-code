using System;
using System.Collections.Generic;
using System.Threading.Tasks;

namespace Fixture.Orders
{
    public sealed class Ledger
    {
        private readonly List<decimal> _entries = new List<decimal>();

        public void Add(decimal amount)
        {
            if (amount < 0) throw new ArgumentOutOfRangeException(nameof(amount));
            _entries.Add(amount);
        }

        public decimal Total()
        {
            decimal sum = 0;
            foreach (var e in _entries)
            {
                sum += e;
            }
            return sum;
        }

        public Task<int> CountAsync() => Task.FromResult(_entries.Count);

        public int Count()
        {
            return CountAsync().Result + CountAsync().GetAwaiter().GetResult();
        }

        public static void Log(string message)
        {
            Console.WriteLine(message);
            Console.Error.WriteLine(message);
        }
    }
}
