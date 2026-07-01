package rs.ctx.agenthistory;

import java.util.List;
import java.util.Map;

/** Import/sync operation payload. */
public final class ImportResult {
    private final Map<String, Object> fields;
    private final Totals totals;
    private final List<Object> sources;

    ImportResult(Map<String, Object> fields) {
        this.fields = AgentHistoryValue.copyObject(fields);
        this.totals = Totals.from(fields.get("totals"));
        this.sources = AgentHistoryValue.rawList(fields.get("sources"));
    }

    static ImportResult from(Object value) {
        return new ImportResult(AgentHistoryValue.object(value));
    }

    public Boolean getResume() {
        return AgentHistoryValue.bool(fields.get("resume"));
    }

    public Boolean resume() {
        return getResume();
    }

    public String getResumeMode() {
        return AgentHistoryValue.string(fields.get("resumeMode"));
    }

    public String resumeMode() {
        return getResumeMode();
    }

    public Totals getTotals() {
        return totals;
    }

    public Totals totals() {
        return totals;
    }

    public List<Object> getSources() {
        return sources;
    }

    public List<Object> sources() {
        return sources;
    }

    public Map<String, Object> asMap() {
        return fields;
    }
}
