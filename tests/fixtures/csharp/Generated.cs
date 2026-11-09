#if NET10_0
using System.Text.Json.Serialization;
namespace Example
{
    public class Payload
    {
        public string Value { get; set; } = "";
    }
    [JsonSerializable(typeof(Payload))]
    internal partial class PayloadContext : JsonSerializerContext { }
}
#endif
