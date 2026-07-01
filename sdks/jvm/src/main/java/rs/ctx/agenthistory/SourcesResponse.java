package rs.ctx.agenthistory;

import java.util.List;
import java.util.Map;

/** Response returned by {@link AgentHistoryClient#sources()}. */
public final class SourcesResponse extends AgentHistoryEnvelope {
    private final List<ProviderSource> sources;

    SourcesResponse(Map<String, Object> canonical) {
        super(canonical);
        this.sources = AgentHistoryValue.objectList(payload("sources"), ProviderSource::new);
    }

    public List<ProviderSource> getSources() {
        return sources;
    }

    public List<ProviderSource> sources() {
        return sources;
    }
}
