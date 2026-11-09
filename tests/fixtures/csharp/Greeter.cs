// π and 😀 exercise conversion from compiler string offsets to UTF-8 bytes.
namespace Example
{
    public static class Greeter
    {
        public static string Greet(string name) => "Hello, " + name;
#if NET10_0
        public static int PlatformSpecific() => 10;
#endif
    }
    public static class Entry
    {
        public static string Run()
        {
            var marker = "π😀";
            return Greeter.Greet(marker);
        }
    }
}
