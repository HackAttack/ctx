package rs.ctx.agenthistory;

import java.util.Map;

/** Aggregate import and refresh counts. */
public final class Totals {
    private final Map<String, Object> fields;

    Totals(Map<String, Object> fields) {
        this.fields = AgentHistoryValue.copyObject(fields);
    }

    static Totals from(Object value) {
        return new Totals(AgentHistoryValue.object(value));
    }

    public Integer getSourceFiles() {
        return AgentHistoryValue.integer(fields.get("sourceFiles"));
    }

    public Integer sourceFiles() {
        return getSourceFiles();
    }

    public Long getSourceBytes() {
        return AgentHistoryValue.longValue(fields.get("sourceBytes"));
    }

    public Long sourceBytes() {
        return getSourceBytes();
    }

    public Integer getImportedSources() {
        return AgentHistoryValue.integer(fields.get("importedSources"));
    }

    public Integer importedSources() {
        return getImportedSources();
    }

    public Integer getFailedSources() {
        return AgentHistoryValue.integer(fields.get("failedSources"));
    }

    public Integer failedSources() {
        return getFailedSources();
    }

    public Integer getImportedSessions() {
        return AgentHistoryValue.integer(fields.get("importedSessions"));
    }

    public Integer importedSessions() {
        return getImportedSessions();
    }

    public Integer getImportedEvents() {
        return AgentHistoryValue.integer(fields.get("importedEvents"));
    }

    public Integer importedEvents() {
        return getImportedEvents();
    }

    public Integer getImportedEdges() {
        return AgentHistoryValue.integer(fields.get("importedEdges"));
    }

    public Integer importedEdges() {
        return getImportedEdges();
    }

    public Integer getSkipped() {
        return AgentHistoryValue.integer(fields.get("skipped"));
    }

    public Integer skipped() {
        return getSkipped();
    }

    public Integer getFailed() {
        return AgentHistoryValue.integer(fields.get("failed"));
    }

    public Integer failed() {
        return getFailed();
    }

    public Map<String, Object> asMap() {
        return fields;
    }
}
