using System.Runtime.CompilerServices;
using System.Security.Cryptography;
using System.Text;
using System.Text.Json;
using KnowCode.Protocol;
using Microsoft.CodeAnalysis;
using Microsoft.CodeAnalysis.CSharp;
using Compilation = Microsoft.CodeAnalysis.Compilation;

// Exact project/CompilationReference identities. Never match by assembly name,
// display signature, output filename, or a guessed target framework.
sealed class ProjectLinks
{
    private sealed record Context(string Id, string Path, TestClassification? Classification, string TargetFamily);
    private sealed record Definition(string Context, string Fingerprint);
    private readonly Dictionary<Compilation, Context> contexts = new(ReferenceEqualityComparer.Instance);
    private readonly Dictionary<IAssemblySymbol, Definition> assemblies = new(SymbolEqualityComparer.Default);
    private static readonly ConditionalWeakTable<SyntaxTree, string> texts = new();
    private static readonly ConditionalWeakTable<Compilation, Dictionary<string,string>> fingerprints = new();
    private readonly Dictionary<Compilation,string> local = new(ReferenceEqualityComparer.Instance);
    private readonly HashSet<Compilation> visiting = new(ReferenceEqualityComparer.Instance);
    private readonly string epoch;
    private readonly uint peerPolicy;
    public List<ProjectReferenceContext> References { get; } = [];
    public string Fingerprint { get; private set; } = "";
    public bool Complete { get; private set; } = true;
    private ProjectLinks(string epoch, uint peerPolicy) { this.epoch=epoch; this.peerPolicy=peerPolicy; }

    public static string ContextId(string root, string path, string target, string configuration, string? platform = null) =>
        $"cs:{System.IO.Path.GetRelativePath(root,path).Replace('\\','/')}:{target}:{configuration}" + (platform is null || platform.Replace(" ", "").Equals("AnyCPU", StringComparison.OrdinalIgnoreCase) ? "" : $":{platform}");

    public static async Task<ProjectLinks> Create(Project project, Compilation compilation, ProjectCatalog catalog, AnalyzeRequest request, string sdk)
    {
        var result=new ProjectLinks($"{request.PreparationFingerprint}:{sdk}:{typeof(CSharpCompilation).Assembly.GetName().Version}"
            + (request.TestClassificationPolicy == 0 ? "" : $":test-classification:{request.TestClassificationPolicy}"), request.TargetEquivalencePolicy);
        // Open() retains only the active dependency closure. These compilations
        // are already needed by the root; no second compiler workspace is built.
        foreach(var candidate in project.Solution.Projects)
        {
            var selected=catalog.Context(candidate.Id);
            var compiled=await candidate.GetCompilationAsync();
            if(selected is null||compiled is null){result.Complete=false;continue;}
            var metadata=catalog.Metadata(candidate.Id);
            var id=ContextId(request.WorkspaceRoot,selected.Value.Path,selected.Value.Target,metadata["Configuration"],
                catalog.BuildContexts.PolicyEnabled ? metadata["Platform"] : null);
            result.contexts[compiled]=new Context(id,System.IO.Path.GetRelativePath(request.WorkspaceRoot,selected.Value.Path).Replace('\\','/'),
                TestClasses.ForContext(catalog.Classification(candidate.Id), request.TestClassificationPolicy, request.WorkspaceRoot, selected.Value.Path),
                TargetPeers.Family(request.TargetEquivalencePolicy, selected.Value.Path, metadata["Configuration"], metadata["Platform"], sdk));
        }
        result.Fingerprint=result.Snapshot(compilation);
        if(result.Fingerprint.Length==0)result.Complete=false;
        var ambiguous=new HashSet<IAssemblySymbol>(SymbolEqualityComparer.Default);
        foreach(var reference in compilation.References.OfType<CompilationReference>())
        {
            if(!result.contexts.TryGetValue(reference.Compilation,out var context)
                ||compilation.GetAssemblyOrModuleSymbol(reference) is not IAssemblySymbol assembly)
            {result.Complete=false;continue;}
            var fingerprint=result.Snapshot(reference.Compilation);
            if(!result.References.Any(r=>r.CompilationId==context.Id))result.References.Add(new ProjectReferenceContext{CompilationId=context.Id,ProjectPath=context.Path,InputFingerprint=fingerprint});
            var definition=new Definition(context.Id,fingerprint);
            if(fingerprint.Length==0 || ambiguous.Contains(assembly) || result.assemblies.TryGetValue(assembly,out var previous)&&previous!=definition)
            {result.Complete=false;result.assemblies.Remove(assembly);ambiguous.Add(assembly);continue;}
            result.assemblies[assembly]=definition;
        }
        return result;
    }

    public (string Compilation, string Symbol, string Fingerprint)? DefinitionOf(ISymbol symbol, Func<ISymbol,string,string> identity)
    {
        if(!Complete || symbol.ContainingAssembly is not { } assembly || symbol.GetDocumentationCommentId() is null
            || !assemblies.TryGetValue(assembly,out var definition))return null;
        return (definition.Context,identity(symbol.OriginalDefinition,definition.Context),definition.Fingerprint);
    }

    private string Snapshot(Compilation compilation)
    {
        if(!contexts.TryGetValue(compilation,out var context)){Complete=false;return "";}
        var cache=fingerprints.GetValue(compilation,_=>[]);
        var tag=$"{epoch}:{context.Id}" + (peerPolicy == 0 ? "" : $":target-equivalence:{peerPolicy}:{context.TargetFamily}");
        if(cache.TryGetValue(tag,out var saved))return saved;
        if(local.TryGetValue(compilation,out saved))return saved;
        if(!visiting.Add(compilation)){Complete=false;return "";}
        using var hash=IncrementalHash.CreateHash(HashAlgorithmName.SHA256);
        void Add(object? value) { hash.AppendData(Encoding.UTF8.GetBytes(JsonSerializer.Serialize(value)));hash.AppendData([0]); }
        Add(tag);Add(compilation.Assembly.Identity.ToString());
        if (context.Classification is { } classification)
            Add(new { classification.PolicyVersion, classification.Kind,
                evidence = classification.Evidence.Select(e => new { e.Basis, e.Value, e.Path, e.Inferred }).ToArray() });
        var options=(CSharpCompilationOptions)compilation.Options;
        Add(new {options.OutputKind,options.ModuleName,options.MainTypeName,options.ScriptClassName,options.Usings,
            options.OptimizationLevel,options.CheckOverflow,options.AllowUnsafe,options.Platform,options.NullableContextOptions,
            options.MetadataImportOptions,options.GeneralDiagnosticOption,options.ConcurrentBuild,options.Deterministic,
            diagnostics=options.SpecificDiagnosticOptions.OrderBy(p=>p.Key).ToArray(),
            sourceResolver=options.SourceReferenceResolver is SourceFileResolver resolver
                ? new {resolver.BaseDirectory,resolver.SearchPaths,resolver.PathMap}:null});
        foreach(var tree in compilation.SyntaxTrees)
        {
            var parse=(CSharpParseOptions)tree.Options;
            Add(tree.FilePath);Add(texts.GetValue(tree,t=>Convert.ToHexString(t.GetText().GetChecksum().AsSpan())));
            Add(new {parse.LanguageVersion,parse.SpecifiedLanguageVersion,parse.Kind,parse.DocumentationMode,
                defines=parse.PreprocessorSymbolNames.ToArray(),features=parse.Features.OrderBy(p=>p.Key).ToArray()});
        }
        foreach(var reference in compilation.References)
        {
            Add(new {reference.Display,reference.Properties.EmbedInteropTypes,aliases=reference.Properties.Aliases.ToArray()});
            if(reference is CompilationReference projectReference)
            {
                var child=Snapshot(projectReference.Compilation);
                if(child.Length==0){visiting.Remove(compilation);return "";}
                Add(child);
            }
            else if(reference is PortableExecutableReference pe)
            {
                using var metadata=pe.GetMetadata();
                Add(metadata is AssemblyMetadata assembly?assembly.GetModules().Select(m=>m.GetModuleVersionId()).ToArray():[((ModuleMetadata)metadata).GetModuleVersionId()]);
            }
        }
        visiting.Remove(compilation);
        var result=Convert.ToHexString(hash.GetHashAndReset()).ToLowerInvariant();
        local[compilation]=result;cache[tag]=result;return result;
    }
}
