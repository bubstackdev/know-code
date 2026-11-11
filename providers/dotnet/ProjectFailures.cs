using System.Security.Cryptography;
using System.Text;
using System.Text.Json;
using KnowCode.Protocol;

// Expected compiler/project setup only. Transport, ingestion, cancellation,
// resource and unknown extraction exceptions must still fail the staged job.
sealed class ProjectSetupException(string projectPath, string code, string message, Exception? inner = null)
    : Exception(message, inner)
{
    public string ProjectPath { get; } = projectPath;
    public string Code { get; } = code;
}
static class ProjectFailures
{
    public static string Bounded(string value, int maximum = 4096)
    {
        if (Encoding.UTF8.GetByteCount(value) <= maximum) return value;
        var output = new StringBuilder(); var bytes = 0;
        foreach (var rune in value.EnumerateRunes()) {
            if (bytes + rune.Utf8SequenceLength > maximum) break;
            output.Append(rune.ToString()); bytes += rune.Utf8SequenceLength;
        }
        return output.ToString();
    }
    public static AnalysisReport SetupReport(ProjectSetupException error, string compilation)
    {
        var report = new AnalysisReport { CompilationId = compilation, Status = "failed" };
        report.Capabilities.Add("project_setup", "unavailable");
        report.Diagnostics.Add(new Diagnostic { Code = error.Code, Severity = "error",
            Message = Bounded(error.Message), Path = Bounded(error.ProjectPath.Replace('\\', '/')) });
        return report;
    }
    public static void Emit(AnalyzeRequest request, ProjectSetupException error, string target = "")
    {
        var relative = Path.GetRelativePath(request.WorkspaceRoot, error.ProjectPath).Replace('\\', '/');
        var identity = Convert.ToHexStringLower(SHA256.HashData(Encoding.UTF8.GetBytes(
            "know-code-unavailable-project-v1\0" + error.ProjectPath + "\0" + target)));
        var id = $"cs:unavailable:{identity}";
        Wire.Send(new Envelope { Compilation = new Compilation { Id = id, Language = "csharp", ProjectPath = relative,
            Target = target, CompilerVersion = "unavailable", InputPaths = { error.ProjectPath },
            TestClassification = request.TestClassificationPolicy == 0 ? null : new TestClassification { PolicyVersion = request.TestClassificationPolicy, Kind = "unknown" },
            TargetEquivalencePolicy = request.TargetEquivalencePolicy,
            TargetFamily = request.TargetEquivalencePolicy == 0 ? "" : identity,
            OptionsJson = JsonSerializer.Serialize(new { setup_failure = error.Code, context = "unavailable", target_known = target.Length != 0 }) } });
        var report = SetupReport(error, id);report.Diagnostics[0].Path = Bounded(relative);
        foreach (var capability in new[] { "calls", "dependencies", "dispatch", "project_links", "target_equivalence" }) report.Capabilities.Add(capability, "unavailable");
        Wire.Send(new Envelope { Report = report });
    }
}
