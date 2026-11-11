using System.Collections.Immutable;
using Microsoft.CodeAnalysis;
using Microsoft.CodeAnalysis.MSBuild;

// Lazy inputs span one job. Only the active dependency closure stays in the
// compiler workspace; shared contexts survive transitions until job disposal.
sealed class ProjectCatalog(BuildContexts buildContexts, SdkMetadataClient metadataClient, IReadOnlySet<string>? unavailable = null) : IDisposable
{
    private readonly AdhocWorkspace catalog = new();
    private readonly AdhocWorkspace active = new();
    private readonly Dictionary<ProjectId, ProjectInfo> inputs = [];
    private readonly Dictionary<(string Path, string Target), ProjectId> contexts = [];
    private readonly Dictionary<ProjectId, ImmutableDictionary<string, string>> compilerMetadata = [];
    private readonly Dictionary<ProjectId, KnowCode.Protocol.TestClassification> testClassifications = [];
    private readonly HashSet<string> loaded = new(StringComparer.Ordinal);
    private readonly List<(HashSet<ProjectId> Owners, WorkspaceDiagnostic Diagnostic)> diagnostics = [];
    private readonly Dictionary<string, string[]> projectInputs = new(StringComparer.Ordinal);
    public BuildContexts BuildContexts => buildContexts;
    public IEnumerable<string> Inputs(string path) => buildContexts.Inputs.Concat(projectInputs.GetValueOrDefault(path, []));

    public async Task Load(string path)
    {
        if (loaded.Contains(path)) return;
        var visiting = new HashSet<string>(StringComparer.Ordinal);
        async Task LoadKnownDependencies(string current)
        {
            if (unavailable?.Contains(current) == true)
                throw new ProjectSetupException(current,"dependency_context_unavailable",$"Compiler-selected project context is unavailable: {current}");
            if (loaded.Contains(current) || !visiting.Add(current)) return;

            var evaluated = await metadataClient.Read(current);
            projectInputs[current] = System.Text.Json.JsonSerializer.Deserialize<string[]>(evaluated.Definition["KnowCodeInputPaths"])!;
            var properties = buildContexts.Properties(current);
            var variants = evaluated.Targets.ToDictionary(t => t.Target, t => t.Identity.ToImmutableDictionary(), StringComparer.Ordinal);

            async Task<bool> LoadVariants(string? selectedTarget)
            {
                var metadata = variants[selectedTarget ?? variants.Keys.First()];
                var selectedProperties = properties.SetItems(metadata.Where(p => p.Value.Length > 0));
                if (selectedTarget is not null) selectedProperties = selectedProperties.Add("TargetFramework", selectedTarget);
                using var loaderWorkspace = new AdhocWorkspace();
                var attemptDiagnostics = new List<WorkspaceDiagnostic>();
                loaderWorkspace.RegisterWorkspaceFailedHandler(e => attemptDiagnostics.Add(e.Diagnostic));
                // Dependencies keep their own SDK identities/compatible target contexts.
                // Exclude every root variant so overlapping outputs cannot alias its IDs.
                async Task<ImmutableArray<ProjectInfo>> ReadInputs()
                {
                    var known = ProjectMap.Create();
                    foreach (var dependency in catalog.CurrentSolution.Projects.Where(p => p.FilePath != current))
                    {
                        if (buildContexts.PolicyEnabled)
                        {
                            if (!compilerMetadata.TryGetValue(dependency.Id, out var dependencyMetadata)) continue;
                            // Roslyn can warn and fall back to the first path-matched
                            // context. Do not offer an incompatible known context.
                            if (!buildContexts.Mapped(current) && (buildContexts.Mapped(dependency.FilePath!)
                                || metadata["Configuration"] != dependencyMetadata["Configuration"]
                                || metadata["Platform"] != dependencyMetadata["Platform"])) continue;
                        }
                        known.Add(dependency);
                    }
                    try { return await new MSBuildProjectLoader(loaderWorkspace, selectedProperties).LoadProjectInfoAsync(current, known); }
                    catch(Exception error) when(error is Microsoft.Build.Exceptions.InvalidProjectFileException or IOException or UnauthorizedAccessException or InvalidOperationException) {
                        throw new ProjectSetupException(current,"project_loading_failed",error.Message,error);
                    }
                }
                var projects = await ReadInputs();
                // Discover through Roslyn's actual compiler inputs. Reload only when
                // new dependencies need their own identity/target contexts first.
                var missing = projects.Select(p => p.FilePath).OfType<string>()
                    .Where(p => p != current && !loaded.Contains(p)).Distinct(StringComparer.Ordinal).ToArray();
                if (missing.Length > 0)
                {
                    foreach (var dependency in missing) await LoadKnownDependencies(dependency);
                    attemptDiagnostics.Clear();
                    projects = await ReadInputs();
                }
                var roots = projects.Where(p => p.FilePath == current).ToArray();
                var selected = new Dictionary<string, ProjectInfo>(StringComparer.Ordinal);
                if (selectedTarget is not null)
                {
                    if (roots.Length != 1) throw new ProjectSetupException(current,"project_context_unavailable",$"cannot select compiler project context for {current} ({selectedTarget})");
                    selected.Add(selectedTarget, roots[0]);
                }
                else
                {
                    // Pinned Roslyn 5.9 names multi-target roots exactly basename(TFM).
                    // Only accept a complete, unambiguous mapping; otherwise load separately.
                    foreach (var target in variants.Keys)
                    {
                        var matches = roots.Where(p => p.Name == $"{Path.GetFileNameWithoutExtension(current)}({target})").ToArray();
                        if (matches.Length != 1) return false;
                        selected.Add(target, matches[0]);
                    }
                    if (roots.Length != selected.Count || selected.Values.Select(p => p.Id).Distinct().Count() != selected.Count) return false;
                }
                var rootNames = selected.ToDictionary(p => p.Value.Id, p => $"{Path.GetFileNameWithoutExtension(current)}({p.Key})");
                foreach (var info in projects)
                {
                    var project = rootNames.TryGetValue(info.Id, out var name) ? info.WithName(name) : info;
                    if (inputs.TryAdd(project.Id, project)) catalog.AddProject(project);
                }
                foreach (var (target, info) in selected)
                {
                    contexts.Add((current, target), info.Id);
                    compilerMetadata.Add(info.Id, variants[target]);
                    testClassifications.Add(info.Id, evaluated.Targets.Single(t => t.Target == target).TestClassification?.Clone()
                        ?? new KnowCode.Protocol.TestClassification { PolicyVersion = TestClasses.Version, Kind = "unknown" });
                }
                var owners = selected.Values.Select(p => p.Id).ToHashSet();
                foreach (var boundary in projects.Where(p => p.FilePath != current && !compilerMetadata.ContainsKey(p.Id))
                    .Select(p => p.FilePath).Distinct(StringComparer.Ordinal))
                    attemptDiagnostics.Add(new WorkspaceDiagnostic(WorkspaceDiagnosticKind.Warning,
                        $"Compiler-selected dependency context for {boundary} is outside the indexed workspace contexts; no project-body link was inferred."));
                diagnostics.AddRange(attemptDiagnostics.Select(d => (owners, d)));
                return true;
            }

            // Metadata is still evaluated for EVERY target. Only identical identity
            // inputs may share one Roslyn load; each compilation stays independent.
            var common = variants.Values.FirstOrDefault();
            var equivalent = common is not null && variants.Values.All(m => m.Count == common.Count && m.All(p => common.TryGetValue(p.Key, out var value) && value == p.Value));
            if (variants.Count <= 1 || !equivalent || !await LoadVariants(null))
                foreach (var target in variants.Keys) await LoadVariants(target);
            loaded.Add(current);
        }
        var originalInputs=inputs.Keys.ToHashSet();var originalContexts=contexts.Keys.ToHashSet();
        var originalMetadata=compilerMetadata.Keys.ToHashSet();var originalClasses=testClassifications.Keys.ToHashSet();
        var originalLoaded=loaded.ToHashSet(StringComparer.Ordinal);var originalProjectInputs=projectInputs.Keys.ToHashSet(StringComparer.Ordinal);
        var originalDiagnostics=diagnostics.Count;
        try {await LoadKnownDependencies(path);}
        catch(ProjectSetupException) {
            // No facts have been emitted. Roll back only this failed lazy-load
            // attempt; previously loaded healthy compiler contexts stay intact.
            var solution=catalog.CurrentSolution;
            foreach(var key in inputs.Keys.Where(key=>!originalInputs.Contains(key)).ToArray()) {inputs.Remove(key);solution=solution.RemoveProject(key);}
            if(!catalog.TryApplyChanges(solution))throw new InvalidOperationException("cannot roll back failed compiler context load");
            foreach(var key in contexts.Keys.Where(key=>!originalContexts.Contains(key)).ToArray())contexts.Remove(key);
            foreach(var key in compilerMetadata.Keys.Where(key=>!originalMetadata.Contains(key)).ToArray())compilerMetadata.Remove(key);
            foreach(var key in testClassifications.Keys.Where(key=>!originalClasses.Contains(key)).ToArray())testClassifications.Remove(key);
            foreach(var key in projectInputs.Keys.Where(key=>!originalProjectInputs.Contains(key)).ToArray())projectInputs.Remove(key);
            loaded.IntersectWith(originalLoaded);diagnostics.RemoveRange(originalDiagnostics,diagnostics.Count-originalDiagnostics);
            throw;
        }
    }

    private HashSet<ProjectId> Closure(string path, string target)
    {
        if (!contexts.TryGetValue((path, target), out var rootId))
            throw new InvalidOperationException($"cannot select compiler project context for {path} ({target})");
        var closure = new HashSet<ProjectId>();
        void Visit(ProjectId id)
        {
            if (!closure.Add(id)) return;
            if (!inputs.TryGetValue(id, out var project)) throw new InvalidOperationException($"referenced compiler project {id} is unavailable for {path}");
            foreach (var reference in project.ProjectReferences) Visit(reference.ProjectId);
        }
        Visit(rootId);
        return closure;
    }

    public IEnumerable<(string Path, string Target)> Schedule(IEnumerable<(string Path, string Target)> requested)
    {
        var pending = requested.Select(r => (r.Path, r.Target, Closure: Closure(r.Path, r.Target))).ToList();
        var current = new HashSet<ProjectId>();
        while (pending.Count > 0)
        {
            var selected = 0;
            var score = -1;
            for (var i = 0; i < pending.Count; i++)
            {
                var shared = pending[i].Closure.Count(current.Contains);
                if (shared > score) { selected = i; score = shared; }
            }
            var next = pending[selected];
            pending.RemoveAt(selected);
            current = next.Closure;
            yield return (next.Path, next.Target);
        }
    }

    public (Project Project, IReadOnlyList<WorkspaceDiagnostic> Diagnostics) Open(string path, string target)
    {
        var closure = Closure(path, target);
        var solution = active.CurrentSolution;
        // Intersections of dependency closures are dependency-closed, so retained
        // projects keep their original references, aliases, and compiler identities.
        foreach (var id in solution.ProjectIds.Where(id => !closure.Contains(id)).ToArray())
            solution = solution.RemoveProject(id);
        foreach (var id in closure.Where(id => solution.GetProject(id) is null))
            solution = solution.AddProject(inputs[id]);
        if (!active.TryApplyChanges(solution)) throw new InvalidOperationException("cannot apply compiler context closure");
        return (active.CurrentSolution.GetProject(contexts[(path, target)])!,
            diagnostics.Where(d => d.Diagnostic is ProjectDiagnostic pd
                    ? closure.Contains(pd.ProjectId) : d.Owners.Overlaps(closure))
                .Select(d => d.Diagnostic).ToArray());
    }

    public IReadOnlyDictionary<string, string> Metadata(ProjectId id) => compilerMetadata[id];
    public KnowCode.Protocol.TestClassification Classification(ProjectId id) => testClassifications[id];

    public (string Path, string Target)? Context(ProjectId id)
    {
        foreach (var (context, projectId) in contexts)
            if (projectId == id) return context;
        return null;
    }

    public void Dispose() { active.Dispose(); catalog.Dispose(); }
}
