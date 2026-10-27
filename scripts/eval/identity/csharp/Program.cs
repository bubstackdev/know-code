// C# references and declarations as identity-v1 enumerates them (eval/identity-v1/README.md), from Roslyn's semantic
// model over MSBuildWorkspace. Runs in the labelling container, after `dotnet restore`.
//
//     Labeller --repo NAME --src DIR --solution FILE > enumeration-NAME.jsonl
using System.Text;
using System.Text.Json;
using Microsoft.Build.Locator;
using Microsoft.CodeAnalysis;
using Microsoft.CodeAnalysis.CSharp;
using Microsoft.CodeAnalysis.CSharp.Syntax;
using Microsoft.CodeAnalysis.MSBuild;

static class Labeller
{
    static string repo = "", src = "";

    static async Task<int> Main(string[] args)
    {
        string? solution = null;
        for (var i = 0; i + 1 < args.Length; i += 2)
        {
            switch (args[i])
            {
                case "--repo": repo = args[i + 1]; break;
                case "--src": src = Path.GetFullPath(args[i + 1]).TrimEnd('/') + "/"; break;
                case "--solution": solution = Path.GetFullPath(args[i + 1]); break;
            }
        }
        if (repo == "" || src == "/" || solution == null)
        {
            Console.Error.WriteLine("usage: Labeller --repo NAME --src DIR --solution FILE");
            return 2;
        }
        MSBuildLocator.RegisterDefaults();
        return await Run(solution);
    }

    static async Task<int> Run(string solutionPath)
    {
        using var workspace = MSBuildWorkspace.Create(new Dictionary<string, string> { ["Configuration"] = "Debug" });
        workspace.RegisterWorkspaceFailedHandler(e => Console.Error.WriteLine($"workspace: {e.Diagnostic.Message}"));
        var solution = await workspace.OpenSolutionAsync(solutionPath);
        var stdout = new StreamWriter(Console.OpenStandardOutput(), new UTF8Encoding(false)) { AutoFlush = false };
        var done = new HashSet<string>();
        var declared = new HashSet<string>();
        // A multi-targeted project loads once per framework: each file is read in the first project that has it.
        foreach (var project in solution.Projects.Where(p => p.Language == LanguageNames.CSharp).OrderBy(p => p.Name, StringComparer.Ordinal))
        {
            var compilation = await project.GetCompilationAsync();
            if (compilation == null) continue;
            foreach (var document in project.Documents.OrderBy(d => d.FilePath, StringComparer.Ordinal))
            {
                var rel = Own(document.FilePath);
                if (rel == null || !done.Add(rel)) continue;
                var tree = await document.GetSyntaxTreeAsync();
                if (tree == null) continue;
                var model = compilation.GetSemanticModel(tree);
                var text = await document.GetTextAsync();
                var root = await tree.GetRootAsync();
                foreach (var node in root.DescendantNodes())
                {
                    var decl = Declared(model, node);
                    if (decl != null)
                    {
                        var key = Key(decl);
                        var group = Group(decl);
                        if (group != null && declared.Add(key))
                        {
                            foreach (var loc in Locations(decl).Take(1))
                                Write(stdout, new Dictionary<string, object?> { ["t"] = "decl", ["repo"] = repo, ["name"] = decl.Name, ["key"] = key, ["file"] = loc.file, ["line"] = loc.line, ["decl"] = group });
                        }
                    }
                    if (node is SimpleNameSyntax name)
                    {
                        var row = Reference(compilation, model, text, rel, name);
                        if (row != null) Write(stdout, row);
                    }
                }
            }
        }
        stdout.Flush();
        return 0;
    }

    // The repository-relative path of one of its own source files; null for build output and generated files.
    static string? Own(string? path)
    {
        if (path == null) return null;
        var full = Path.GetFullPath(path);
        if (!full.StartsWith(src, StringComparison.Ordinal)) return null;
        var rel = full.Substring(src.Length);
        var parts = rel.Split('/');
        if (parts.Any(p => p == "obj" || p == "bin")) return null;
        if (rel.EndsWith(".g.cs", StringComparison.Ordinal) || rel.EndsWith(".Designer.cs", StringComparison.Ordinal)) return null;
        return rel;
    }

    static ISymbol? Declared(SemanticModel model, SyntaxNode node) => node switch
    {
        BaseTypeDeclarationSyntax or DelegateDeclarationSyntax or MethodDeclarationSyntax or PropertyDeclarationSyntax
            or EventDeclarationSyntax or EnumMemberDeclarationSyntax => model.GetDeclaredSymbol(node),
        VariableDeclaratorSyntax v when v.Parent?.Parent is BaseFieldDeclarationSyntax => model.GetDeclaredSymbol(v),
        _ => null,
    };

    // What a declaration counts as for multiplicity: null for what isn't counted.
    static string? Group(ISymbol s) => s switch
    {
        INamedTypeSymbol t when t.TypeKind is TypeKind.Class or TypeKind.Struct or TypeKind.Interface or TypeKind.Enum or TypeKind.Delegate => "type",
        IMethodSymbol m when m.MethodKind is MethodKind.Ordinary or MethodKind.ExplicitInterfaceImplementation => "callable",
        IPropertySymbol p when !p.IsIndexer => "member",
        IFieldSymbol or IEventSymbol => "member",
        _ => null,
    };

    static string Key(ISymbol s) => s.GetDocumentationCommentId() ?? s.ToDisplayString();

    static IEnumerable<(string file, int line)> Locations(ISymbol s) =>
        s.Locations.Where(l => l.IsInSource).Select(l => (Own(l.SourceTree?.FilePath), l.GetLineSpan().StartLinePosition.Line + 1))
            .Where(x => x.Item1 != null).Select(x => (x.Item1!, x.Item2)).Distinct().OrderBy(x => x.Item1, StringComparer.Ordinal).ThenBy(x => x.Item2);

    static Dictionary<string, object?>? Reference(Compilation compilation, SemanticModel model, Microsoft.CodeAnalysis.Text.SourceText text, string rel, SimpleNameSyntax node)
    {
        if (node.IsVar || InUsingOrNamespace(node) || IsConstructed(node)) return null;
        var symbol = model.GetSymbolInfo(node).Symbol;
        if (symbol == null || symbol.IsImplicitlyDeclared) return null;
        if (symbol is IMethodSymbol reduced && reduced.ReducedFrom != null) symbol = reduced.ReducedFrom;
        symbol = symbol.OriginalDefinition;
        var token = node.Identifier.ValueText;
        if (token != symbol.Name) return null;
        string kind;
        switch (symbol)
        {
            case IMethodSymbol m when m.MethodKind is MethodKind.Ordinary:
                kind = "call"; break;
            case IPropertySymbol p when !p.IsIndexer:
            case IFieldSymbol:
            case IEventSymbol:
                kind = "member"; break;
            case INamedTypeSymbol t when t.TypeKind is TypeKind.Class or TypeKind.Struct or TypeKind.Interface or TypeKind.Enum or TypeKind.Delegate && !t.IsAnonymousType:
                kind = "type"; break;
            default:
                return null;
        }
        if (kind != "type")
        {
            var container = symbol.ContainingType;
            if (symbol is IMethodSymbol { IsExtensionMethod: true } || container?.TypeKind.ToString() == "Extension")
                kind = "extension";
            else if (container?.TypeKind == TypeKind.Interface || symbol.IsAbstract)
                kind = "dispatch";
        }
        var span = node.Identifier.Span;
        var position = text.Lines.GetLinePosition(span.Start);
        var lineText = text.Lines[position.Line].ToString();
        var col = Encoding.UTF8.GetByteCount(lineText.Substring(0, position.Character)) + 1;
        var row = new Dictionary<string, object?>
        {
            ["t"] = "ref", ["repo"] = repo, ["file"] = rel, ["line"] = position.Line + 1, ["col"] = col, ["name"] = token,
            ["kind"] = kind, ["key"] = Key(symbol), ["symbol"] = symbol.ToDisplayString(),
        };
        var inSource = symbol.Locations.Where(l => l.IsInSource).ToList();
        if (inSource.Count > 0)
        {
            var locs = Locations(symbol).Select(l => new object[] { l.file, l.line }).ToList();
            // Declared in a generated file or outside the repository's sources: not labelled.
            if (locs.Count == 0 || locs.Count < inSource.Count) return null;
            row["inside"] = true; row["locs"] = locs; row["origin"] = null;
        }
        else
        {
            row["inside"] = false; row["locs"] = new List<object>(); row["origin"] = Origin(compilation, symbol);
        }
        return row;
    }

    // `stdlib`: the .NET base class library (the Microsoft.NETCore.App reference pack); `sdk`: another shared
    // framework's reference pack; `package`: a NuGet package.
    static string Origin(Compilation compilation, ISymbol symbol)
    {
        var assembly = symbol.ContainingAssembly;
        var reference = assembly == null ? null : compilation.GetMetadataReference(assembly) as PortableExecutableReference;
        var path = reference?.FilePath ?? "";
        if (path.Contains("/Microsoft.NETCore.App.Ref/") || path.Contains("/NETStandard.Library.Ref/") || path.Contains("/netstandard.library/")) return "stdlib";
        if (path.Contains("/packs/")) return "sdk";
        return "package";
    }

    static bool InUsingOrNamespace(SyntaxNode node)
    {
        for (var n = node.Parent; n != null; n = n.Parent)
        {
            if (n is UsingDirectiveSyntax) return true;
            if (n is BaseNamespaceDeclarationSyntax ns) return ns.Name.Span.Contains(node.Span);
            if (n is StatementSyntax || n is MemberDeclarationSyntax && n is not BaseNamespaceDeclarationSyntax) return false;
        }
        return false;
    }

    // The type named by `new Foo(…)` or an attribute: the toolchain binds the construction to a constructor.
    static bool IsConstructed(SimpleNameSyntax node)
    {
        SyntaxNode n = node;
        while (n.Parent is QualifiedNameSyntax q && q.Right == n) n = q;
        while (n.Parent is AliasQualifiedNameSyntax a && a.Name == n) n = a;
        return n.Parent is ObjectCreationExpressionSyntax o && o.Type == n || n.Parent is AttributeSyntax at && at.Name == n;
    }

    static void Write(StreamWriter w, Dictionary<string, object?> row)
    {
        w.Write(JsonSerializer.Serialize(new SortedDictionary<string, object?>(row, StringComparer.Ordinal)));
        w.Write('\n');
    }
}
