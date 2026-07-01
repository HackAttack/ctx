using System.Text.Json.Nodes;

namespace Ctx.AgentHistory;

public sealed record VersionInfo(
    string SdkVersion,
    string ApiVersion,
    string Transport,
    string? CtxVersion)
{
    public JsonObject ToJsonObject()
    {
        return new JsonObject
        {
            ["sdkVersion"] = SdkVersion,
            ["apiVersion"] = ApiVersion,
            ["transport"] = Transport,
            ["ctxVersion"] = CtxVersion
        };
    }
}
