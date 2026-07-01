using System.Text.Json.Nodes;

namespace Ctx.AgentHistory;

/// <summary>Hosted agent-history-v1 placeholder. It performs no network I/O.</summary>
public sealed class HostedAdapter : IAgentHistoryTransport
{
    public HostedAdapter(HostedAgentHistoryConfig config)
    {
        Config = config;
    }

    public string Name => "hosted";
    public HostedAgentHistoryConfig Config { get; }

    public JsonObject Backend(JsonObject? raw = null)
    {
        var backend = new JsonObject { ["kind"] = "hosted" };
        if (!string.IsNullOrWhiteSpace(Config.BaseUrl))
        {
            backend["baseUrl"] = Config.BaseUrl;
        }
        return backend;
    }

    public Task<JsonObject> ExecuteJsonAsync(
        string operation,
        IReadOnlyList<string> args,
        CancellationToken cancellationToken = default)
    {
        throw new HostedTransportNotImplementedException(operation, Config);
    }

    public Task<string?> GetCtxVersionAsync(CancellationToken cancellationToken = default)
    {
        return Task.FromResult<string?>(null);
    }
}
