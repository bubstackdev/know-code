using System.Security.Cryptography;
using System.Text.Json;
using Microsoft.CodeAnalysis;
using Microsoft.CodeAnalysis.CSharp;

// Explicit compiler/evaluation evidence; never recover a family or a
// declaration from the display spelling of a concrete public ID.
static class TargetPeers
{
    public const uint Policy = 1;
    public static void Validate(uint policy)
    {
        if (policy > Policy) throw new InvalidDataException($"update required: unsupported target equivalence policy {policy}");
    }
    public static string Family(uint policy, string project, string configuration, string platform, string sdk)
        => policy == 0 ? "" : Hash(new { domain = "know-code-csharp-target-family-v1",
            project = Path.GetFullPath(project), configuration, platform, sdk,
            compiler = typeof(CSharpCompilation).Assembly.GetName().Version });
    public static string Declaration(uint policy, IAssemblySymbol? assembly, string identity)
        => policy == 0 || assembly is null ? "" : Hash(new { domain = "know-code-csharp-declaration-v1",
            assembly = assembly.Identity.ToString(), identity });
    private static string Hash<T>(T value) => Convert.ToHexStringLower(SHA256.HashData(JsonSerializer.SerializeToUtf8Bytes(value)));
}
