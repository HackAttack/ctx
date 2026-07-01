package rs.ctx.agenthistory;

import java.util.Map;

/** Search filter metadata. Unknown additive filters remain available through asMap(). */
public final class SearchFilters {
    private final Map<String, Object> fields;

    SearchFilters(Map<String, Object> fields) {
        this.fields = AgentHistoryValue.copyObject(fields);
    }

    static SearchFilters from(Object value) {
        Map<String, Object> fields = AgentHistoryValue.objectOrNull(value);
        return fields == null ? null : new SearchFilters(fields);
    }

    public String getProvider() {
        return AgentHistoryValue.string(fields.get("provider"));
    }

    public String provider() {
        return getProvider();
    }

    public String getWorkspace() {
        return AgentHistoryValue.string(fields.get("workspace"));
    }

    public String workspace() {
        return getWorkspace();
    }

    public String getSince() {
        return AgentHistoryValue.string(fields.get("since"));
    }

    public String since() {
        return getSince();
    }

    public Map<String, Object> asMap() {
        return fields;
    }
}
