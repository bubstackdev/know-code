using System.Diagnostics;
using System.Reflection;
using System.Runtime.CompilerServices;
using System.Text;
using KnowCode.Protocol;
using Microsoft.Build.Evaluation;
using Microsoft.Build.Execution;
using Microsoft.Build.Framework;

// One request-local cache, shared by preparation and the lazy Roslyn catalog.
// No SDK process or compilation remains resident between projects/jobs.
sealed class SdkMetadataClient(BuildContexts contexts, string sdkPath)
{
    private readonly Dictionary<string, SdkMetadataResult> complete = new(StringComparer.Ordinal);

    public async Task<SdkMetadataResult> Read(string path, bool includeIdentity = true)
    {
        var properties = contexts.Properties(path);
        var key = System.Text.Json.JsonSerializer.Serialize(new {path, properties = properties.OrderBy(p => p.Key).ToArray()});
        if (includeIdentity && complete.TryGetValue(key, out var cached)) return cached;
        var info = new ProcessStartInfo("dotnet") { WorkingDirectory = Path.GetDirectoryName(path)!,
            RedirectStandardInput = true, RedirectStandardOutput = true, RedirectStandardError = true };
        info.ArgumentList.Add(Assembly.GetExecutingAssembly().Location);
        info.ArgumentList.Add("--sdk-metadata");
        using var process = Process.Start(info) ?? throw new InvalidOperationException("cannot start SDK metadata helper; install the required .NET runtime");
        var errors = Tools.Capture(process.StandardError, false, info.WorkingDirectory, 4096);
        SdkMetadataResult? result = null;
        var completed = false;
        try
        {
            var request = new SdkMetadataRequest { SdkPath = sdkPath, ProjectPath = path,
                Configuration = properties.GetValueOrDefault("Configuration", ""), IncludeIdentity = includeIdentity };
            if (properties.TryGetValue("Platform", out var platform)) request.Platform = platform;
            request.SolutionProperties.Add(properties.Where(p => p.Key is not ("Configuration" or "Platform"))
                .ToDictionary(p => p.Key, p => p.Value));
            Wire.Write(process.StandardInput.BaseStream, new Envelope { ProtocolVersion = Wire.ProtocolVersion, JobId = Wire.JobId,
                SdkMetadataRequest = request });
            process.StandardInput.Close();
            await Task.Run(() => {
                while (Wire.Read(process.StandardOutput.BaseStream) is { } message)
                {
                    if (message.ProtocolVersion != Wire.ProtocolVersion || message.JobId != Wire.JobId)
                        throw new InvalidDataException("update required: incompatible SDK metadata helper");
                    if (completed) throw new InvalidDataException("SDK metadata arrived after completion");
                    if (message.Progress is not null) Wire.Send(message);
                    else if (message.SdkMetadataResult is not null && result is null) result = message.SdkMetadataResult;
                    else if (message.Complete?.Success == true) completed = true;
                    else throw new InvalidDataException("unexpected SDK metadata helper message");
                }
            });
            await process.WaitForExitAsync();
            var details = await errors;
            if (process.ExitCode != 0)
                throw new InvalidOperationException($"SDK metadata failed for {path} ({process.ExitCode}): {details}");
            if (!completed || result is null) throw new InvalidDataException($"SDK metadata incomplete for {path}: {details}");
            if (includeIdentity) complete.Add(key, result);
            return result;
        }
        catch
        {
            if (!process.HasExited) process.Kill(entireProcessTree: true);
            await process.WaitForExitAsync();
            await errors;
            throw;
        }
    }
}

static class SdkMetadataServer
{
    // Locator registration must happen before this method's SDK types are JITted.
    [MethodImpl(MethodImplOptions.NoInlining)]
    public static void Run(SdkMetadataRequest request)
    {
        using var collection = new ProjectCollection();
        try
        {
            var globals = new Dictionary<string, string>(StringComparer.OrdinalIgnoreCase);
            if (!string.IsNullOrEmpty(request.Configuration)) globals.Add("Configuration", request.Configuration);
            if (request.HasPlatform) globals.Add("Platform", request.Platform);
            foreach (var property in request.SolutionProperties)
            {
                if (property.Key is not ("BuildingSolutionFile" or "CurrentSolutionConfigurationContents" or "SolutionDir"
                    or "SolutionPath" or "SolutionFileName" or "SolutionName" or "SolutionExt"))
                    throw new InvalidDataException($"unsupported solution property: {property.Key}");
                globals.Add(property.Key, property.Value);
            }
            var inputPaths = new HashSet<string>(StringComparer.Ordinal);
            var outer = new Microsoft.Build.Evaluation.Project(request.ProjectPath, globals, null, collection);
            var result = new SdkMetadataResult();
            foreach (var name in new[] { "TargetFrameworks", "TargetFramework", "UsingMicrosoftNETSdk", "Configuration", "Platform" })
                result.Definition.Add(name, outer.GetPropertyValue(name));
            inputPaths.UnionWith(outer.Imports.Select(i => i.ImportedProject.FullPath));
            var frameworks = result.Definition["TargetFrameworks"];
            if (string.IsNullOrWhiteSpace(frameworks)) frameworks = result.Definition["TargetFramework"];
            var targets = frameworks.Split(';', StringSplitOptions.RemoveEmptyEntries | StringSplitOptions.TrimEntries).Distinct(StringComparer.Ordinal).ToArray();
            if (targets.Length == 0) throw new InvalidOperationException($"cannot determine target frameworks for {request.ProjectPath}");
            foreach (var target in targets)
            {
                // Fresh evaluation/built state for every inner context. Metadata
                // hooks may change imported files, so clear XML state between targets.
                var properties = new Dictionary<string, string>(globals) { ["TargetFramework"] = target };
                var project = new Microsoft.Build.Evaluation.Project(request.ProjectPath, properties, null, collection);
                var state = project.CreateProjectInstance();
                var assets = state.GetPropertyValue("ProjectAssetsFile");
                if (string.IsNullOrWhiteSpace(assets)) assets = Path.Combine(state.GetPropertyValue("MSBuildProjectExtensionsPath"), "project.assets.json");
                var metadata = new SdkTargetMetadata { Target = target,
                    AssetsFile = Path.GetFullPath(assets, Path.GetDirectoryName(request.ProjectPath)!) };
                inputPaths.UnionWith(project.Imports.Select(i => i.ImportedProject.FullPath));
                if (request.IncludeIdentity)
                    foreach (var name in new[] { "Configuration", "Platform" })
                        metadata.Identity.Add(name, state.GetPropertyValue(name));
                if (request.IncludeIdentity && result.Definition["UsingMicrosoftNETSdk"].Equals("true", StringComparison.OrdinalIgnoreCase))
                {
                    Wire.Send(new Envelope { Progress = new Progress { Stage = "preparing", ProjectPath = request.ProjectPath,
                        Message = $"Evaluating SDK assembly identity for {target}" } });
                    var logger = new Errors();
                    using var manager = new BuildManager();
                    // Standard metadata target only; never Build/CoreCompile/codegen.
                    var built = manager.Build(new BuildParameters(collection) {
                        MaxNodeCount = 1, EnableNodeReuse = false, ShutdownInProcNodeOnBuildFinish = true,
                        DiscardBuildResults = true, Loggers = [logger]
                    }, new BuildRequestData(state, ["GetAssemblyVersion"], null, BuildRequestDataFlags.ProvideProjectStateAfterBuild));
                    if (built.OverallResult != BuildResultCode.Success || built.ProjectStateAfterBuild is null)
                        throw new InvalidOperationException($"GetAssemblyVersion failed for {request.ProjectPath} ({target}): {logger.Text}", built.Exception);
                    foreach (var name in new[] { "Version", "AssemblyVersion", "FileVersion", "InformationalVersion" })
                        metadata.Identity.Add(name, built.ProjectStateAfterBuild.GetPropertyValue(name));
                }
                result.Targets.Add(metadata);
                collection.UnloadAllProjects();
            }
            result.Definition.Add("KnowCodeInputPaths", System.Text.Json.JsonSerializer.Serialize(inputPaths.Order(StringComparer.Ordinal)));
            Wire.Send(new Envelope { SdkMetadataResult = result });
        }
        finally { collection.UnloadAllProjects(); }
    }

    private sealed class Errors : ILogger
    {
        private readonly StringBuilder text = new();
        public string Text => text.ToString();
        public LoggerVerbosity Verbosity { get; set; } = LoggerVerbosity.Quiet;
        public string? Parameters { get; set; }
        public void Initialize(IEventSource source) => source.ErrorRaised += (_, error) => {
            text.AppendLine($"{error.File}({error.LineNumber},{error.ColumnNumber}): {error.Code}: {error.Message}");
            if (text.Length > 4096) text.Remove(0, text.Length - 4096);
        };
        public void Shutdown() { }
    }
}
