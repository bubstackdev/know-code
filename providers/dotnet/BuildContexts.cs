using System.Collections.Immutable;
using System.Text.Json;
using System.Xml.Linq;
using KnowCode.Protocol;

// One selected workspace context. MSBuild owns solution parsing/defaults and
// mapping semantics; this adapter carries the selected globals to all phases.
sealed class BuildContexts
{
    private sealed record Selection(string Configuration, string Platform);
    private readonly AnalyzeRequest request;
    private readonly Dictionary<string, Selection> mapped = new(StringComparer.Ordinal);
    private readonly ImmutableDictionary<string, string> solutionProperties;
    public bool PolicyEnabled => request.AnalysisPolicy is not null;
    public string? SolutionPath { get; }
    public string? SolutionConfiguration { get; }
    public string? SolutionPlatform { get; }
    public string? RequestedConfiguration { get; }
    public IReadOnlyList<string> InputPaths { get; }

    public static readonly string[] SolutionPropertyNames = ["BuildingSolutionFile", "CurrentSolutionConfigurationContents",
        "SolutionDir", "SolutionPath", "SolutionFileName", "SolutionName", "SolutionExt"];

    private BuildContexts(AnalyzeRequest request, string? solutionPath, SdkMetadataResult? solution)
    {
        this.request = request;
        RequestedConfiguration = PolicyEnabled
            ? request.AnalysisPolicy.Csharp.HasConfiguration ? request.AnalysisPolicy.Csharp.Configuration : null
            : request.Configuration;
        SolutionPath = solutionPath;
        InputPaths = solutionPath is null ? [] : [solutionPath];
        solutionProperties = ImmutableDictionary<string, string>.Empty;
        if (solution is null) return;
        SolutionConfiguration = solution.Definition["Configuration"];
        SolutionPlatform = solution.Definition["Platform"];
        var contents = solution.Definition["CurrentSolutionConfigurationContents"];
        if (string.IsNullOrWhiteSpace(contents))
            throw new InvalidOperationException($"solution has no selected context {SolutionConfiguration}|{SolutionPlatform}: {solutionPath}");
        foreach (var project in XElement.Parse(contents).Elements("ProjectConfiguration"))
        {
            var selected = project.Value;
            var separator = selected.LastIndexOf('|');
            if (separator <= 0 || separator == selected.Length - 1)
                throw new InvalidDataException($"invalid SDK solution mapping: {selected}");
            var path = project.Attribute("AbsolutePath")?.Value
                ?? throw new InvalidDataException("SDK solution mapping has no project path");
            mapped.Add(Path.GetFullPath(path), new(selected[..separator], selected[(separator + 1)..]));
        }
        solutionProperties = SolutionPropertyNames.ToImmutableDictionary(name => name, name => solution.Definition[name]);
        InputPaths = InputPaths.Concat(JsonSerializer.Deserialize<string[]>(solution.Definition["KnowCodeInputPaths"])!)
            .Distinct(StringComparer.Ordinal).ToArray();
    }

    public static void Validate(AnalyzeRequest request)
    {
        if (request.AnalysisPolicy is not { } policy) return;
        if (policy.Version != 1 || policy.SettingsCase != AnalysisPolicy.SettingsOneofCase.Csharp)
            throw new InvalidOperationException("update required: C# worker supports C# analysis policy 1");
        if (policy.Csharp.HasConfiguration && string.IsNullOrWhiteSpace(policy.Csharp.Configuration))
            throw new InvalidOperationException("C# configuration override must be nonempty");
        if (policy.Csharp.HasSolutionPath && !Path.IsPathFullyQualified(policy.Csharp.SolutionPath))
            throw new InvalidOperationException("selected solution_path must be absolute");
    }

    public static async Task<BuildContexts> Create(AnalyzeRequest request, string sdkPath)
    {
        Validate(request);
        if (request.AnalysisPolicy is null) return new(request, null, null);
        var policy = request.AnalysisPolicy.Csharp;
        var candidates = policy.HasSolutionPath ? [policy.SolutionPath] : Discover(request.WorkspaceRoot).Take(2).ToArray();
        if (candidates.Length > 1)
            throw new InvalidOperationException("multiple solution files require one workspace solution_path selection; no solution contexts were expanded");
        if (candidates.Length == 0) return new(request, null, null);
        var selected = Path.GetFullPath(candidates[0]);
        if (!File.Exists(selected)) throw new InvalidOperationException($"selected solution is unavailable: {selected}");
        var source = selected;
        if (Path.GetExtension(selected).Equals(".slnf", StringComparison.OrdinalIgnoreCase))
        {
            using var filter = JsonDocument.Parse(File.ReadAllText(selected));
            source = Path.GetFullPath(filter.RootElement.GetProperty("solution").GetProperty("path").GetString()!
                .Replace('\\', Path.DirectorySeparatorChar), Path.GetDirectoryName(selected)!);
        }
        if (Path.GetExtension(source) is not (".sln" or ".slnx"))
            throw new InvalidOperationException($"selected file is not a supported .sln, .slnx or .slnf solution: {selected}");
        var configuration = policy.HasConfiguration ? policy.Configuration : null;
        var evaluated = await SdkMetadataClient.ReadSolution(source, configuration, sdkPath);
        var result = new BuildContexts(request, source, evaluated);
        if (source != selected) result.filterPath = selected;
        return result;
    }
    private string? filterPath;

    private static IEnumerable<string> Discover(string root)
    {
        var excluded = new HashSet<string>([".git", ".research", ".artifacts", "node_modules", "obj", "bin", "target", ".build", ".worktrees", "DerivedData"], StringComparer.Ordinal);
        var pending = new Stack<string>(); pending.Push(root);
        while (pending.TryPop(out var directory))
            foreach (var item in Directory.EnumerateFileSystemEntries(directory).Order(StringComparer.Ordinal))
            {
                var attributes = File.GetAttributes(item);
                if ((attributes & FileAttributes.Directory) != 0)
                {
                    if ((attributes & FileAttributes.ReparsePoint) == 0 && !excluded.Contains(Path.GetFileName(item)) && !Path.GetFileName(item).StartsWith(".build-", StringComparison.Ordinal)) pending.Push(item);
                }
                else if (Path.GetExtension(item) is ".sln" or ".slnx" or ".slnf") yield return item;
            }
    }

    public ImmutableDictionary<string, string> Properties(string path)
    {
        var properties = mapped.ContainsKey(path) ? solutionProperties : ImmutableDictionary<string, string>.Empty;
        if (mapped.TryGetValue(path, out var selected))
            return properties.Add("Configuration", selected.Configuration).Add("Platform", selected.Platform);
        return RequestedConfiguration is null ? properties : properties.Add("Configuration", RequestedConfiguration);
    }

    public bool Mapped(string path) => mapped.ContainsKey(path);
    public IEnumerable<string> Inputs => filterPath is null ? InputPaths : InputPaths.Append(filterPath);
    public string Origin(string path) => mapped.ContainsKey(path) ? "solution_mapping"
        : PolicyEnabled ? RequestedConfiguration is null ? "repository_default" : "workspace_override" : "legacy_workspace_configuration";
    public object Provenance(string path) => new { origin = Origin(path), requested_configuration = RequestedConfiguration,
        solution_path = SolutionPath, selected_solution_path = filterPath ?? SolutionPath, solution_configuration = SolutionConfiguration, solution_platform = SolutionPlatform };
}
