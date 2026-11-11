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
    public static string Family(uint policy, string project, string configuration, string platform, string sdk, Platform compilerPlatform)
        => policy == 0 ? "" : Hash(new { domain = "know-code-csharp-target-family-v1",
            project = Path.GetFullPath(project), configuration, platform, sdk, compilerPlatform,
            compiler = typeof(CSharpCompilation).Assembly.GetName().Version });
    public static string Declaration(uint policy, IAssemblySymbol? assembly, string identity)
        => policy == 0 || assembly is null || identity.Length == 0 ? "" : Hash(new { domain = "know-code-csharp-declaration-v1",
            assembly = assembly.Identity.ToString(), identity });
    // Public IDs retain their existing recipe. Peer evidence additionally binds
    // every scoped fallback to its compiler-selected containing declaration.
    public static string SymbolDeclaration(uint policy, ISymbol symbol, Func<SyntaxTree, bool> owned)
        => policy == 0 ? "" : Declaration(policy, symbol.ContainingAssembly, OwnerIdentity(symbol, owned) ?? "");
    public static string SyntheticDeclaration(uint policy, IAssemblySymbol assembly, ISymbol? owner,
        SyntaxNode node, string kind, Func<SyntaxTree, bool> owned)
    {
        if (policy == 0 || owner is null || !owned(node.SyntaxTree)) return "";
        var identity = OwnerIdentity(owner, owned);
        return identity is null ? "" : Declaration(policy, assembly, JsonSerializer.Serialize(new {
            owner = identity, kind, path = node.SyntaxTree.FilePath, start = node.SpanStart, length = node.Span.Length }));
    }
    private sealed record Anchor(string Path, int Start, int Length);
    private static string? OwnerIdentity(ISymbol symbol, Func<SyntaxTree, bool> owned)
    {
        var chain = new List<object>();
        for (ISymbol? current = symbol; current is not null; current = current.ContainingSymbol)
        {
            if (current.Locations.Any(location => location.IsInSource && !owned(location.SourceTree!))) return null;
            var documentation = current.GetDocumentationCommentId();
            var scoped = current.Kind is SymbolKind.Local or SymbolKind.Parameter or SymbolKind.RangeVariable
                || current is IMethodSymbol { MethodKind: MethodKind.LocalFunction or MethodKind.AnonymousFunction };
            if (!scoped && documentation is not null)
            {
                if (chain.Count == 0) return documentation;
                chain.Add(new { documentation });
                return JsonSerializer.Serialize(chain);
            }
            var anchors = current.Locations.Where(location => location.IsInSource && owned(location.SourceTree!))
                .Select(location => new Anchor(location.SourceTree!.FilePath, location.SourceSpan.Start, location.SourceSpan.Length))
                .Distinct().ToArray();
            if (anchors.Length == 0)
                anchors = current.DeclaringSyntaxReferences.Where(reference => owned(reference.SyntaxTree))
                    .Select(reference => new Anchor(reference.SyntaxTree.FilePath, reference.Span.Start, reference.Span.Length))
                    .Distinct().ToArray();
            if (anchors.Length != 1) return null;
            chain.Add(new { kind = current.Kind.ToString(), current.Name, anchor = anchors[0] });
        }
        return null;
    }
    private static string Hash<T>(T value) => Convert.ToHexStringLower(SHA256.HashData(JsonSerializer.SerializeToUtf8Bytes(value)));
}
