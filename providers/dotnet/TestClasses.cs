using System.Text;
using KnowCode.Protocol;

// Saved project-context evidence only. No package/path heuristics or test runs.
static class TestClasses
{
    public const uint Version = 1;
    public static readonly string[] Properties = ["IsTestProject", "IsTestingPlatformApplication"];
    public static void Validate(uint policy)
    {
        if (policy is not (0 or Version))
            throw new InvalidDataException($"update required: unsupported test classification policy {policy}; this provider supports {Version}");
    }
    public static TestClassification From(IReadOnlyDictionary<string, string> properties,
        string project, string target, string stage)
    {
        var result = new TestClassification { PolicyVersion = Version, Kind = "unknown" };
        var flags = new bool?[Properties.Length];
        for (var i = 0; i < Properties.Length; i++)
        {
            if (!properties.TryGetValue(Properties[i], out var raw)) continue;
            var value = $"{Properties[i]}={raw}; target={target}; stage={stage}";
            if (Encoding.UTF8.GetByteCount(value) <= 512 && bool.TryParse(raw, out var parsed)) flags[i] = parsed;
            result.Evidence.Add(new TestEvidence { Basis = "msbuild_property", Value = Bounded(value, 512),
                Path = Bounded(project.Replace('\\', '/'), 4096), Inferred = false });
        }
        if (Encoding.UTF8.GetByteCount(project) <= 4096)
            result.Kind = flags[0] switch {
                true => "test",
                false => flags[1] == true ? "unknown" : "non_test",
                null => flags[1] == true ? "test" : "unknown"
            };
        return result;
    }
    public static TestClassification? ForContext(TestClassification? value, uint policy, string root, string project)
    {
        if (policy == 0) return null;
        var result = value?.Clone() ?? new TestClassification { PolicyVersion = Version, Kind = "unknown" };
        var path = Path.GetRelativePath(root, project).Replace('\\', '/');
        if (Encoding.UTF8.GetByteCount(path) > 4096) result.Kind = "unknown";
        foreach (var evidence in result.Evidence) evidence.Path = Bounded(path, 4096);
        return result;
    }
    public static string Capability(TestClassification? value) => value?.Kind is "test" or "non_test"
        ? "complete" : value?.Evidence.Count > 0 ? "partial" : "unknown";
    private static string Bounded(string text, int maximum)
    {
        if (Encoding.UTF8.GetByteCount(text) <= maximum) return text;
        var result = new StringBuilder();var count = 0;
        foreach (var rune in text.EnumerateRunes())
        {
            if (count + rune.Utf8SequenceLength > maximum - 3) break;
            result.Append(rune.ToString());count += rune.Utf8SequenceLength;
        }
        return result.Append("...").ToString();
    }
}
