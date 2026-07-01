package rs.ctx.agenthistory;

import java.util.Map;

/** Search pagination metadata. */
public final class SearchPagination {
    private final Map<String, Object> fields;

    SearchPagination(Map<String, Object> fields) {
        this.fields = AgentHistoryValue.copyObject(fields);
    }

    static SearchPagination from(Object value) {
        Map<String, Object> fields = AgentHistoryValue.objectOrNull(value);
        return fields == null ? null : new SearchPagination(fields);
    }

    public Integer getLimit() {
        return AgentHistoryValue.integer(fields.get("limit"));
    }

    public Integer limit() {
        return getLimit();
    }

    public Integer getOffset() {
        return AgentHistoryValue.integer(fields.get("offset"));
    }

    public Integer offset() {
        return getOffset();
    }

    public Integer getTotal() {
        return AgentHistoryValue.integer(fields.get("total"));
    }

    public Integer total() {
        return getTotal();
    }

    public String getNextCursor() {
        return AgentHistoryValue.string(fields.get("nextCursor"));
    }

    public String nextCursor() {
        return getNextCursor();
    }

    public Boolean getHasMore() {
        return AgentHistoryValue.bool(fields.get("hasMore"));
    }

    public Boolean hasMore() {
        return getHasMore();
    }

    public Map<String, Object> asMap() {
        return fields;
    }
}
