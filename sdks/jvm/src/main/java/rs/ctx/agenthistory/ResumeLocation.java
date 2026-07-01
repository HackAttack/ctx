package rs.ctx.agenthistory;

import java.util.Map;

/** Resume metadata returned by locate operations. */
public final class ResumeLocation {
    private final Map<String, Object> fields;

    ResumeLocation(Map<String, Object> fields) {
        this.fields = AgentHistoryValue.copyObject(fields);
    }

    static ResumeLocation from(Object value) {
        Map<String, Object> fields = AgentHistoryValue.objectOrNull(value);
        return fields == null ? null : new ResumeLocation(fields);
    }

    public String getCursor() {
        return AgentHistoryValue.string(fields.get("cursor"));
    }

    public String cursor() {
        return getCursor();
    }

    public String getPath() {
        return AgentHistoryValue.string(fields.get("path"));
    }

    public String path() {
        return getPath();
    }

    public Map<String, Object> asMap() {
        return fields;
    }
}
