package rs.ctx.agenthistory;

import java.util.Map;

/** Local agent history index status. */
public final class StatusRecord {
    private final Map<String, Object> fields;
    private final Freshness freshness;

    StatusRecord(Map<String, Object> fields) {
        this.fields = AgentHistoryValue.copyObject(fields);
        this.freshness = Freshness.from(fields.get("freshness"));
    }

    static StatusRecord from(Object value) {
        return new StatusRecord(AgentHistoryValue.object(value));
    }

    public Boolean getInitialized() {
        return AgentHistoryValue.bool(fields.get("initialized"));
    }

    public Boolean initialized() {
        return getInitialized();
    }

    public Boolean getLocalOnly() {
        return AgentHistoryValue.bool(fields.get("localOnly"));
    }

    public Boolean localOnly() {
        return getLocalOnly();
    }

    public String getDataRoot() {
        return AgentHistoryValue.string(fields.get("dataRoot"));
    }

    public String dataRoot() {
        return getDataRoot();
    }

    public Integer getIndexedItems() {
        return AgentHistoryValue.integer(fields.get("indexedItems"));
    }

    public Integer indexedItems() {
        return getIndexedItems();
    }

    public Integer getIndexedSources() {
        return AgentHistoryValue.integer(fields.get("indexedSources"));
    }

    public Integer indexedSources() {
        return getIndexedSources();
    }

    public Integer getCatalogedSessions() {
        return AgentHistoryValue.integer(fields.get("catalogedSessions"));
    }

    public Integer catalogedSessions() {
        return getCatalogedSessions();
    }

    public Integer getIndexedCatalogSessions() {
        return AgentHistoryValue.integer(fields.get("indexedCatalogSessions"));
    }

    public Integer indexedCatalogSessions() {
        return getIndexedCatalogSessions();
    }

    public Integer getPendingCatalogSessions() {
        return AgentHistoryValue.integer(fields.get("pendingCatalogSessions"));
    }

    public Integer pendingCatalogSessions() {
        return getPendingCatalogSessions();
    }

    public Integer getFailedCatalogSessions() {
        return AgentHistoryValue.integer(fields.get("failedCatalogSessions"));
    }

    public Integer failedCatalogSessions() {
        return getFailedCatalogSessions();
    }

    public Integer getStaleCatalogSessions() {
        return AgentHistoryValue.integer(fields.get("staleCatalogSessions"));
    }

    public Integer staleCatalogSessions() {
        return getStaleCatalogSessions();
    }

    public Freshness getFreshness() {
        return freshness;
    }

    public Freshness freshness() {
        return freshness;
    }

    public Map<String, Object> getSemantic() {
        return AgentHistoryValue.objectAt(fields, "semantic");
    }

    public Map<String, Object> semantic() {
        return getSemantic();
    }

    public Map<String, Object> getDaemon() {
        return AgentHistoryValue.objectAt(fields, "daemon");
    }

    public Map<String, Object> daemon() {
        return getDaemon();
    }

    public Map<String, Object> asMap() {
        return fields;
    }
}
