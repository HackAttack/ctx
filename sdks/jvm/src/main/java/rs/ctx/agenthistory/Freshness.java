package rs.ctx.agenthistory;

import java.util.Map;
import java.util.List;

/** Optional pre-search refresh metadata. */
public final class Freshness {
    private final Map<String, Object> fields;
    private final Totals totals;

    Freshness(Map<String, Object> fields) {
        this.fields = AgentHistoryValue.copyObject(fields);
        this.totals = fields.containsKey("totals") ? Totals.from(fields.get("totals")) : null;
    }

    static Freshness from(Object value) {
        Map<String, Object> fields = AgentHistoryValue.objectOrNull(value);
        return fields == null ? null : new Freshness(fields);
    }

    public String getMode() {
        return AgentHistoryValue.string(fields.get("mode"));
    }

    public String mode() {
        return getMode();
    }

    public String getStatus() {
        return AgentHistoryValue.string(fields.get("status"));
    }

    public String status() {
        return getStatus();
    }

    public String getReason() {
        return AgentHistoryValue.string(fields.get("reason"));
    }

    public String reason() {
        return getReason();
    }

    public List<String> getBudgetReasons() {
        return AgentHistoryValue.stringList(fields.get("budgetReasons"));
    }

    public List<String> budgetReasons() {
        return getBudgetReasons();
    }

    public Integer getSourceCount() {
        return AgentHistoryValue.integer(fields.get("sourceCount"));
    }

    public Integer sourceCount() {
        return getSourceCount();
    }

    public Long getDaemonLastRunAtMs() {
        return AgentHistoryValue.longValue(fields.get("daemonLastRunAtMs"));
    }

    public Long daemonLastRunAtMs() {
        return getDaemonLastRunAtMs();
    }

    public Totals getTotals() {
        return totals;
    }

    public Totals totals() {
        return totals;
    }

    public String getError() {
        return AgentHistoryValue.string(fields.get("error"));
    }

    public String error() {
        return getError();
    }

    public Map<String, Object> asMap() {
        return fields;
    }
}
