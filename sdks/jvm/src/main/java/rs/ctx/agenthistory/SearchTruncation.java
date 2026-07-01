package rs.ctx.agenthistory;

import java.util.Map;

/** Search truncation metadata. */
public final class SearchTruncation {
    private final Map<String, Object> fields;

    SearchTruncation(Map<String, Object> fields) {
        this.fields = AgentHistoryValue.copyObject(fields);
    }

    static SearchTruncation from(Object value) {
        Map<String, Object> fields = AgentHistoryValue.objectOrNull(value);
        return fields == null ? null : new SearchTruncation(fields);
    }

    public Boolean getTruncated() {
        return AgentHistoryValue.bool(fields.get("truncated"));
    }

    public Boolean truncated() {
        return getTruncated();
    }

    public String getReason() {
        return AgentHistoryValue.string(fields.get("reason"));
    }

    public String reason() {
        return getReason();
    }

    public Integer getMaxResults() {
        return AgentHistoryValue.integer(fields.get("maxResults"));
    }

    public Integer maxResults() {
        return getMaxResults();
    }

    public Integer getMaxBytes() {
        return AgentHistoryValue.integer(fields.get("maxBytes"));
    }

    public Integer maxBytes() {
        return getMaxBytes();
    }

    public Map<String, Object> asMap() {
        return fields;
    }
}
