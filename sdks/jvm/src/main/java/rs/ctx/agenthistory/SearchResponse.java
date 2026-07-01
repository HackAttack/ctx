package rs.ctx.agenthistory;

import java.util.Map;

/** Response returned by {@link AgentHistoryClient#search(AgentHistoryOptions.Search)}. */
public final class SearchResponse extends AgentHistoryEnvelope {
    private final SearchResult search;

    SearchResponse(Map<String, Object> canonical) {
        super(canonical);
        this.search = SearchResult.from(payload("search"));
    }

    public SearchResult getSearch() {
        return search;
    }

    public SearchResult search() {
        return search;
    }
}
