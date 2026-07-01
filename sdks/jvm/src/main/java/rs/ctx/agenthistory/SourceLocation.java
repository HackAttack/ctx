package rs.ctx.agenthistory;

import java.util.Map;

/** Source provenance for show and locate results. */
public final class SourceLocation {
    private final Map<String, Object> fields;

    SourceLocation(Map<String, Object> fields) {
        this.fields = AgentHistoryValue.copyObject(fields);
    }

    static SourceLocation from(Object value) {
        Map<String, Object> fields = AgentHistoryValue.objectOrNull(value);
        return fields == null ? null : new SourceLocation(fields);
    }

    public String getPath() {
        return AgentHistoryValue.string(fields.get("path"));
    }

    public String path() {
        return getPath();
    }

    public String getCursor() {
        return AgentHistoryValue.string(fields.get("cursor"));
    }

    public String cursor() {
        return getCursor();
    }

    public Boolean getExists() {
        return AgentHistoryValue.bool(fields.get("exists"));
    }

    public Boolean exists() {
        return getExists();
    }

    public String getSourceId() {
        return AgentHistoryValue.string(fields.get("sourceId"));
    }

    public String sourceId() {
        return getSourceId();
    }

    public String getSourceFormat() {
        return AgentHistoryValue.string(fields.get("sourceFormat"));
    }

    public String sourceFormat() {
        return getSourceFormat();
    }

    public Map<String, Object> asMap() {
        return fields;
    }
}
