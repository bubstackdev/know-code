using System.Diagnostics;
using System.Reflection;
using System.Security.Cryptography;
using System.Text;
using System.Text.Json;
using Google.Protobuf;
using KnowCode.Protocol;
using Microsoft.Build.Locator;
using Microsoft.CodeAnalysis;
using Microsoft.CodeAnalysis.CSharp;
using Microsoft.CodeAnalysis.CSharp.Syntax;
using Microsoft.CodeAnalysis.MSBuild;
using Diagnostic = KnowCode.Protocol.Diagnostic;

try
{
    // Managed task/generator console output must never corrupt the Protobuf stream.
    // Wire owns a raw standard-output stream independently of Console.Out.
    Console.SetOut(Console.Error);
    var request = Wire.Read();
    if (request.ProtocolVersion != Wire.ProtocolVersion)
        throw new InvalidOperationException("update required: incompatible worker protocol");
    Wire.JobId = request.JobId;
    if (args is ["--sdk-metadata"])
    {
        var metadata = request.SdkMetadataRequest ?? throw new InvalidDataException("missing SDK metadata request");
        Environment.CurrentDirectory = Path.GetDirectoryName(metadata.ProjectPath)!;
        MSBuildLocator.RegisterMSBuildPath(metadata.SdkPath);
        SdkMetadataServer.Run(metadata);
    }
    else
    {
        if (args.Length != 0 || request.Analyze is null) throw new InvalidDataException("invalid analysis worker request");
        TestClasses.Validate(request.Analyze.TestClassificationPolicy);
        TargetPeers.Validate(request.Analyze.TargetEquivalencePolicy);
        BuildContexts.Validate(request.Analyze);
        var projects = request.Analyze.Projects;
        if (projects.Count == 0) throw new InvalidOperationException("no C# projects requested");
        var selected = await Tools.Dotnet(Path.GetDirectoryName(projects[0])!, "--version");
        var sdkList = await Tools.Dotnet(request.Analyze.WorkspaceRoot, "--list-sdks");
        var sdkLine = sdkList.Split('\n').FirstOrDefault(line => line.StartsWith(selected.Trim() + " [", StringComparison.Ordinal))
            ?? throw new InvalidOperationException($"selected SDK {selected.Trim()} is not installed");
        var sdkPath = Path.Combine(sdkLine[(sdkLine.IndexOf('[') + 1)..sdkLine.LastIndexOf(']')], selected.Trim());
        var sdkCompiler = AssemblyName.GetAssemblyName(Path.Combine(sdkPath, "Roslyn/bincore/Microsoft.CodeAnalysis.CSharp.dll")).Version;
        var providerCompiler = typeof(CSharpCompilation).Assembly.GetName().Version;
        if (sdkCompiler != providerCompiler)
            throw new InvalidOperationException($"unsupported SDK compiler {sdkCompiler}; this provider uses Roslyn {providerCompiler}. Update the Know Code provider; no SDK fallback was used.");
        var sdkSelections = new HashSet<string>(StringComparer.Ordinal) { Tools.SdkSelectionKey(projects[0]) };
        foreach (var project in projects.Skip(1))
        {
            if (!sdkSelections.Add(Tools.SdkSelectionKey(project))) continue;
            var version = await Tools.Dotnet(Path.GetDirectoryName(project)!, "--version");
            if (version.Trim() != selected.Trim())
                throw new InvalidOperationException("multiple SDK selections in one workspace require separate worker sessions; unsupported in this first slice");
        }
        MSBuildLocator.RegisterMSBuildPath(sdkPath);
        Wire.Send(new Envelope { Provider = new ProviderInfo { Language = "csharp", CompilerVersion = $"Roslyn {providerCompiler}; SDK {selected.Trim()}", RuntimeVersion = Environment.Version.ToString(), AnalysisPolicyVersion = request.Analyze.AnalysisPolicy is null ? 0u : 1u,
            TestClassificationPolicy = request.Analyze.TestClassificationPolicy, TargetEquivalencePolicy = request.Analyze.TargetEquivalencePolicy } });
        await Analysis.Run(request.Analyze, selected.Trim(), sdkPath);
    }
    Wire.Send(new Envelope { Complete = new Completion { Success = true } });
}
catch (Exception error)
{
    Console.Error.WriteLine($"C# analysis failed: {error}");
    Environment.ExitCode = 1;
}

static class Wire
{
    public const int ProtocolVersion = 7;
    public const int MaxFrame = 8 * 1024 * 1024;
    public static string JobId = "";
    private static readonly Stream Output = Console.OpenStandardOutput();
    private static readonly object Gate = new();
    public static Envelope Read()
    {
        using var input = Console.OpenStandardInput();
        return Read(input) ?? throw new EndOfStreamException("missing worker request");
    }
    public static Envelope? Read(Stream input)
    {
        Span<byte> header = stackalloc byte[4];
        var count = input.Read(header);
        if (count == 0) return null;
        input.ReadExactly(header[count..]);
        var size = System.Buffers.Binary.BinaryPrimitives.ReadInt32LittleEndian(header);
        if (size <= 0 || size > MaxFrame) throw new InvalidDataException("invalid request frame length");
        var bytes = new byte[size];
        input.ReadExactly(bytes);
        return Envelope.Parser.ParseFrom(bytes);
    }
    public static void Send(Envelope envelope)
    {
        envelope.ProtocolVersion = ProtocolVersion;
        envelope.JobId = JobId;
        lock (Gate) Write(Output, envelope);
    }
    public static void Write(Stream output, Envelope envelope)
    {
        var bytes = envelope.ToByteArray();
        if (bytes.Length == 0 || bytes.Length > MaxFrame)
            throw new InvalidDataException($"resource-limited: frame exceeds {MaxFrame} bytes");
        Span<byte> header = stackalloc byte[4];
        System.Buffers.Binary.BinaryPrimitives.WriteInt32LittleEndian(header, bytes.Length);
        output.Write(header);
        output.Write(bytes);
        output.Flush();
    }
}

static class Tools
{
    // SDK selection depends on the nearest global.json (and the shared process
    // environment). Recheck each distinct selection scope, including nested pins.
    public static string SdkSelectionKey(string project)
    {
        for (var directory = new DirectoryInfo(Path.GetDirectoryName(project)!); directory is not null; directory = directory.Parent)
        {
            var path = Path.Combine(directory.FullName, "global.json");
            if (File.Exists(path)) return path;
        }
        return "";
    }

    public static Task<string> Dotnet(string directory, params string[] arguments) => Run(directory, false, arguments);
    public static Task<string> Restore(string directory, params string[] arguments) => Run(directory, true, arguments);
    private static async Task<string> Run(string directory, bool progress, string[] arguments)
    {
        var info = new ProcessStartInfo("dotnet") { WorkingDirectory = directory, RedirectStandardOutput = true, RedirectStandardError = true };
        foreach (var argument in arguments) info.ArgumentList.Add(argument);
        using var process = Process.Start(info) ?? throw new InvalidOperationException("cannot start dotnet; install the required SDK");
        var stdout = Capture(process.StandardOutput, progress, directory, progress ? 4096 : 1024 * 1024);
        var stderr = Capture(process.StandardError, progress, directory, progress ? 4096 : 1024 * 1024);
        await process.WaitForExitAsync();
        var output = await stdout;
        var errors = await stderr;
        if (process.ExitCode != 0) throw new InvalidOperationException($"dotnet {string.Join(' ', arguments)} failed ({process.ExitCode}): {output}\n{errors}");
        return output;
    }
    public static async Task<string> Capture(StreamReader reader, bool progress, string directory, int limit)
    {
        var tail = new StringBuilder();
        var buffer = new char[1024];
        var last = Environment.TickCount64;
        int count;
        while ((count = await reader.ReadAsync(buffer)) != 0)
        {
            tail.Append(buffer, 0, count);
            if (tail.Length > limit) tail.Remove(0, tail.Length - limit);
            if (progress && Environment.TickCount64 - last >= 500)
            {
                last = Environment.TickCount64;
                Wire.Send(new Envelope { Progress = new Progress { Stage = "restoring", ProjectPath = directory, Message = new string(buffer, 0, Math.Min(count, 512)) } });
            }
        }
        return tail.ToString();
    }
}

static partial class Analysis
{
    public static async Task Run(AnalyzeRequest request, string sdkVersion, string sdkPath)
    {
        var contexts = await BuildContexts.Create(request, sdkPath);
        var metadata = new SdkMetadataClient(contexts, sdkPath);
        var preparationErrors = new Dictionary<string, string>(StringComparer.Ordinal);
        // Finish restoration before caching any compiler inputs or identity metadata.
        foreach (var projectPath in request.Projects)
        {
            var key = $"nuget:{projectPath}";
            var globals = contexts.Properties(projectPath);
            var plan = await metadata.Read(projectPath, includeIdentity: false);
            var fingerprint = Convert.ToHexStringLower(SHA256.HashData(Encoding.UTF8.GetBytes(
                JsonSerializer.Serialize(new { request.PreparationFingerprint, sdkVersion,
                    configuration = plan.Definition["Configuration"], platform = plan.Definition["Platform"],
                    assets = plan.Targets.Select(t => t.AssetsFile).ToArray(), globals = globals.OrderBy(p => p.Key).ToArray() }))));
            var previous = request.Preparations.FirstOrDefault(p => p.Key == key && p.Fingerprint == fingerprint);
            var preparationError = "";
            if (previous is not null && !request.RetryPreparation && (!previous.Succeeded || previous.Outputs.Count > 0 && previous.Outputs.All(File.Exists)))
                preparationError = previous.Error;
            else
            {
                var assets = plan.Targets.Select(t => t.AssetsFile).ToHashSet(StringComparer.Ordinal);
                if (previous?.Succeeded != true || request.RetryPreparation || assets.Any(p => !File.Exists(p)))
                {
                    Wire.Send(new Envelope { Progress = new Progress { Stage = "restoring", ProjectPath = projectPath, Message = "Restoring NuGet dependencies for all declared targets" } });
                    try { await Tools.Restore(request.WorkspaceRoot, ["restore", projectPath, "--nologo",
                        .. globals.Select(p => $"-property:{p.Key}={Microsoft.Build.Evaluation.ProjectCollection.Escape(p.Value)}")]); }
                    catch (InvalidOperationException error) { preparationError = error.Message; }
                }
                if (preparationError.Length > 4096) preparationError = preparationError[^4096..];
                Wire.Send(new Envelope { Preparation = new PreparationResult { Key = key, Fingerprint = fingerprint,
                    Succeeded = preparationError.Length == 0, Error = preparationError, Outputs = { assets } } });
            }
            preparationErrors.Add(projectPath, preparationError);
        }
        using var catalog = new ProjectCatalog(contexts, metadata);
        var requested = new List<(string Path, string Target)>();
        foreach (var projectPath in request.Projects)
        {
            var evaluated = await metadata.Read(projectPath);
            await catalog.Load(projectPath);
            requested.AddRange(evaluated.Targets.Select(t => (projectPath, t.Target)));
        }
        foreach (var (projectPath, target) in catalog.Schedule(requested))
        {
            Wire.Send(new Envelope { Progress = new Progress { Stage = "analyzing", ProjectPath = projectPath, Message = $"Analyzing {target} ({contexts.Origin(projectPath)})" } });
            await Project(request, projectPath, target, sdkVersion, preparationErrors[projectPath], catalog);
        }
    }

    private static async Task Project(AnalyzeRequest request, string projectPath, string target, string sdkVersion, string preparationError, ProjectCatalog catalog)
    {
        var relative = Path.GetRelativePath(request.WorkspaceRoot, projectPath).Replace('\\', '/');
        var opened = catalog.Open(projectPath, target);
        var project = opened.Project;
        var compilerMetadata = catalog.Metadata(project.Id);
        var configuration = compilerMetadata["Configuration"];
        var platform = compilerMetadata["Platform"];
        var classification = TestClasses.ForContext(catalog.Classification(project.Id), request.TestClassificationPolicy,
            request.WorkspaceRoot, projectPath);
        var id = ProjectLinks.ContextId(request.WorkspaceRoot, projectPath, target, configuration,
            catalog.BuildContexts.PolicyEnabled ? platform : null);
        var diagnostics = new List<Diagnostic>();
        if (preparationError.Length > 0) diagnostics.Add(new Diagnostic { Code = "dependency_restore_failed", Severity = "error", Message = preparationError, Path = projectPath });
        diagnostics.AddRange(opened.Diagnostics.Select(d => new Diagnostic { Code = "project_loading", Severity = d.Kind == WorkspaceDiagnosticKind.Failure ? "error" : "warning", Message = d.Message }));
        var compilation = await project.GetCompilationAsync() ?? throw new InvalidOperationException($"compilation unavailable for {projectPath} ({target})");
        var projectLinks = await ProjectLinks.Create(project, compilation, catalog, request, sdkVersion);
        Wire.Send(new Envelope { Compilation = new KnowCode.Protocol.Compilation {
            InputFingerprint = projectLinks.Fingerprint, References = {projectLinks.References},
            TestClassification = classification,
            TargetEquivalencePolicy = request.TargetEquivalencePolicy,
            TargetFamily = TargetPeers.Family(request.TargetEquivalencePolicy, projectPath, configuration, platform, sdkVersion, compilation.Options.Platform),
            Id = id, Language = "csharp", ProjectPath = relative, Target = target, Configuration = configuration,
            CompilerVersion = $"Roslyn {typeof(CSharpCompilation).Assembly.GetName().Version}; SDK {sdkVersion}",
            Dependencies = { project.ProjectReferences.Select(r => project.Solution.GetProject(r.ProjectId)?.FilePath).Where(p => p is not null).Select(p => Path.GetRelativePath(request.WorkspaceRoot, p!).Replace('\\', '/')) },
            InputPaths = { project.AdditionalDocuments.Select(d => d.FilePath).Concat(project.AnalyzerConfigDocuments.Select(d => d.FilePath))
                .Concat(project.Documents.Where(d => GeneratedFile(d.FilePath)).Select(d => d.FilePath)).OfType<string>()
                .Concat(catalog.Inputs(projectPath)).Distinct(StringComparer.Ordinal) },
            OptionsJson = JsonSerializer.Serialize(new { sdk = sdkVersion, target, configuration, platform,
                build_context = catalog.BuildContexts.Provenance(projectPath),
                project_context = project.Name, compiler_metadata = catalog.Metadata(project.Id), assembly_identity = compilation.Assembly.Identity.ToString(),
                references = project.ProjectReferences.Select(r => project.Solution.GetProject(r.ProjectId)!).Select(p => new {
                    project_path = Path.GetRelativePath(request.WorkspaceRoot, p.FilePath!).Replace('\\', '/'), context = p.Name }) }) } });
        var documents = new List<(Document Document, bool Generated)>();
        documents.AddRange(project.Documents.Select(d => (d, GeneratedFile(d.FilePath))));
        documents.AddRange((await project.GetSourceGeneratedDocumentsAsync()).Select(d => ((Document)d, true)));
        var sources = new Dictionary<SyntaxTree, string>();
        foreach (var (document, generated) in documents)
        {
            var tree = await document.GetSyntaxTreeAsync();
            if (tree is null || sources.ContainsKey(tree)) continue;
            var text = (await document.GetTextAsync()).ToString();
            var utf8 = Encoding.UTF8.GetBytes(text);
            var hash = Convert.ToHexStringLower(SHA256.HashData(utf8));
            var name = generated
                ? $"$generated/{id}/{document.FilePath ?? document.Name}"
                : Path.GetRelativePath(request.WorkspaceRoot, document.FilePath ?? document.Name);
            name = name.Replace('\\', '/');
            var artifact = $"{name}@{hash}";
            sources.Add(tree, artifact);
            Wire.Send(new Envelope { Source = new SourceArtifact { Id = artifact, Path = name, Utf8 = ByteString.CopyFrom(utf8), Sha256 = hash, Generated = generated, CompilationId = id } });
        }
        var emitter = new Emitter(id, sources, projectLinks, request.TargetEquivalencePolicy, compilation.Assembly);
        foreach (var (tree, artifactId) in sources)
        {
            var model = compilation.GetSemanticModel(tree);
            var root = await tree.GetRootAsync();
            // Keep one file's conversion map resident, not a map for every source.
            var source = new Source(artifactId, Offsets((await tree.GetTextAsync()).ToString()));
            var graph = new GraphFacts(model, source, emitter);
            foreach (var node in root.DescendantNodesAndSelf())
            {
                if (node is MemberDeclarationSyntax or VariableDeclaratorSyntax or ParameterSyntax or TypeParameterSyntax
                    or SingleVariableDesignationSyntax or LocalFunctionStatementSyntax or CatchDeclarationSyntax or ForEachStatementSyntax)
                {
                    var declared = model.GetDeclaredSymbol(node);
                    if (declared is not null)
                    {
                        graph.Declaration(node, declared);
                        var symbol = Normalize(declared);
                        var symbolId = emitter.Symbol(symbol);
                        foreach (var location in declared.Locations.Where(l => l.IsInSource && l.SourceTree == tree && node.Span.Contains(l.SourceSpan)))
                            emitter.Occurrence(symbolId, source, location.SourceSpan, "declaration", node.Span);
                    }
                }
                if (node is SimpleNameSyntax name)
                {
                    var symbol = ReferenceInfo(model, name).Symbol;
                    // CandidateSymbols are not exact bindings and must not be promoted.
                    if (symbol is not null)
                    {
                        emitter.Occurrence(emitter.Symbol(Normalize(symbol)), source, name.Identifier.Span, "reference");
                        graph.Reference(name, symbol);
                    }
                }
                graph.Operations(node);
            }
            emitter.EndFile();
        }
        emitter.Flush();
        var compilerDiagnostics = compilation.GetDiagnostics();
        diagnostics.AddRange(compilerDiagnostics.Where(d => d.Severity is DiagnosticSeverity.Error or DiagnosticSeverity.Warning).Take(100)
            .Select(d => new Diagnostic { Code = d.Id, Severity = d.Severity == DiagnosticSeverity.Error ? "error" : "warning", Message = d.GetMessage(), Path = d.Location.SourceTree?.FilePath ?? "" }));
        if (compilerDiagnostics.Length > 100) diagnostics.Add(new Diagnostic { Code = "diagnostics_truncated", Severity = "info", Message = $"{compilerDiagnostics.Length} diagnostics; at most 100 included." });
        var report = new AnalysisReport { CompilationId = id,
            Status = compilerDiagnostics.Any(d => d.Severity == DiagnosticSeverity.Error) || diagnostics.Any(d => d.Code == "project_loading" || d.Severity == "error") ? "partial" : "complete" };
        report.Capabilities.Add("calls", "partial");
        report.Capabilities.Add("dependencies", "partial");
        report.Capabilities.Add("dispatch", "partial");
        report.Capabilities.Add("project_links", projectLinks.Complete ? "complete" : "partial");
        if (request.TestClassificationPolicy != 0)
            report.Capabilities.Add("test_classification", TestClasses.Capability(classification));
        if (request.TargetEquivalencePolicy != 0)
            report.Capabilities.Add("target_equivalence", documents.Any(d => d.Generated) || !emitter.TargetEquivalenceComplete ? "partial" : "complete");
        report.Diagnostics.AddRange(diagnostics);
        Wire.Send(new Envelope { Report = report });
    }

    private static bool GeneratedFile(string? filename) => filename?.Split(Path.DirectorySeparatorChar).Contains("obj") == true;

    private static SymbolInfo ReferenceInfo(SemanticModel model, SimpleNameSyntax name)
    {
        var expression = name.Parent switch
        {
            MemberAccessExpressionSyntax access when access.Name == name => (ExpressionSyntax)access,
            MemberBindingExpressionSyntax binding when binding.Name == name => binding,
            _ => name
        };
        if (expression.Parent is InvocationExpressionSyntax invocation && invocation.Expression == expression)
        {
            // The call supplies the chosen method without constructing the name's
            // whole method group. A delegate call instead refers to its variable/member.
            var call = model.GetSymbolInfo(invocation);
            if (call.Symbol is IMethodSymbol { MethodKind: MethodKind.Ordinary or MethodKind.ReducedExtension or MethodKind.LocalFunction })
                return call;
        }
        return model.GetSymbolInfo(name);
    }

    private static ISymbol Normalize(ISymbol symbol) => symbol is IMethodSymbol { ReducedFrom: not null } method
        ? method.ReducedFrom.OriginalDefinition : symbol.OriginalDefinition;

    private static int[] Offsets(string text)
    {
        var offsets = new int[text.Length + 1];
        var bytes = 0;
        for (var i = 0; i < text.Length;)
        {
            var valid = Rune.TryGetRuneAt(text, i, out var rune);
            if (!valid) rune = Rune.ReplacementChar;
            offsets[i] = bytes;
            var size = valid ? rune.Utf16SequenceLength : 1;
            if (size == 2) offsets[i + 1] = bytes;
            bytes += rune.Utf8SequenceLength;
            i += size;
        }
        offsets[text.Length] = bytes;
        return offsets;
    }

    private sealed record Source(string Id, int[] Offsets);
    private sealed class Emitter(string compilationId, Dictionary<SyntaxTree, string> sources, ProjectLinks projectLinks, uint peerPolicy, IAssemblySymbol assembly)
    {
        private readonly HashSet<string> emitted = [];
        // A reference cache lasts one source file, so it cannot retain semantic
        // models/binders from earlier files or merge merely equivalent symbols.
        private readonly Dictionary<ISymbol, string> fileIds = new(ReferenceEqualityComparer.Instance);
        public void EndFile() => fileIds.Clear();
        private FactBatch batch = new() { CompilationId = compilationId };
        public bool TargetEquivalenceComplete { get; private set; } = true;
        private static string DeclarationIdentity(ISymbol symbol)
        {
            var key = symbol.GetDocumentationCommentId();
            if (key is null)
            {
                var declaration = symbol.Locations.FirstOrDefault(l => l.IsInSource);
                var anchor = declaration is null ? "metadata" : $"{declaration.SourceTree!.FilePath}:{declaration.SourceSpan.Start}";
                key = $"{symbol.Kind}:{anchor}:{symbol.Name}";
            }
            return key;
        }
        private static string Identity(ISymbol symbol, string context)
        {
            return $"{context}:{symbol.ContainingAssembly?.Identity}:{DeclarationIdentity(symbol)}";
        }
        public string Symbol(ISymbol symbol)
        {
            if (fileIds.TryGetValue(symbol, out var cached)) return cached;
            var id = Identity(symbol, compilationId);
            fileIds.Add(symbol, id);
            if (emitted.Add(id))
            {
                var definition=projectLinks.DefinitionOf(symbol,Identity);
                var external = !symbol.Locations.Any(l => l.SourceTree is not null && sources.ContainsKey(l.SourceTree));
                var declarationKey = external ? "" : TargetPeers.SymbolDeclaration(peerPolicy, symbol, sources.ContainsKey);
                if (peerPolicy != 0 && !external && declarationKey.Length == 0) TargetEquivalenceComplete = false;
                batch.Symbols.Add(new KnowCode.Protocol.Symbol { Id = id, Name = symbol is IMethodSymbol {MethodKind: MethodKind.AnonymousFunction} ? "<lambda>" : symbol.Name,
                    Kind = symbol is IMethodSymbol {MethodKind: MethodKind.AnonymousFunction} ? "lambda" : symbol is INamedTypeSymbol type ? type.TypeKind.ToString().ToLowerInvariant() : symbol.Kind.ToString().ToLowerInvariant(),
                    Signature = symbol.ToDisplayString(SymbolDisplayFormat.CSharpErrorMessageFormat),
                    External = external,
                    DeclarationKey = declarationKey,
                    DefinitionCompilationId=definition?.Compilation ?? "", DefinitionSymbolId=definition?.Symbol ?? "", DefinitionFingerprint=definition?.Fingerprint ?? "" });
            }
            return id;
        }
        public void Occurrence(string symbolId, Source source, Microsoft.CodeAnalysis.Text.TextSpan span, string role, Microsoft.CodeAnalysis.Text.TextSpan? extent = null)
        {
            batch.Occurrences.Add(new Occurrence { SymbolId = symbolId, ArtifactId = source.Id,
                StartByte = (ulong)source.Offsets[span.Start], EndByte = (ulong)source.Offsets[span.End], Role = role });
            if (extent is { } full)
                batch.Occurrences.Add(new Occurrence { SymbolId = symbolId, ArtifactId = source.Id,
                    StartByte = (ulong)source.Offsets[full.Start], EndByte = (ulong)source.Offsets[full.End], Role = "definition" });
            if (batch.Occurrences.Count >= 128) Flush();
        }
        public string Synthetic(Source source, SyntaxNode node, string kind, string name, ISymbol? owner = null)
        {
            var id=$"{compilationId}:${kind}:{node.SyntaxTree.FilePath}:{node.SpanStart}";
            if(emitted.Add(id))
            {
                var declarationKey = TargetPeers.SyntheticDeclaration(peerPolicy, assembly, owner, node, kind, sources.ContainsKey);
                if (peerPolicy != 0 && declarationKey.Length == 0) TargetEquivalenceComplete = false;
                batch.Symbols.Add(new KnowCode.Protocol.Symbol { Id=id,Name=name,Kind=kind,Signature=name, DeclarationKey=declarationKey });
                Occurrence(id,source,node.Span,"declaration",node.Span);
            }
            return id;
        }
        public void Relationship(Relationship relationship)
        {
            batch.Relationships.Add(relationship);
            if(batch.Relationships.Count>=128)Flush();
        }
        public void Flush()
        {
            if (batch.Symbols.Count != 0 || batch.Occurrences.Count != 0 || batch.Relationships.Count != 0) Wire.Send(new Envelope { Facts = batch });
            batch = new FactBatch { CompilationId = compilationId };
        }
    }
}
