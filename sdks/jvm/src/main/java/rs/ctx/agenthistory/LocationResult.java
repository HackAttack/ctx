package rs.ctx.agenthistory;

import java.util.Map;

/** Locate-event/session payload. */
public final class LocationResult {
    private final Map<String, Object> fields;
    private final SourceLocation source;
    private final ResumeLocation resume;

    LocationResult(Map<String, Object> fields) {
        this.fields = AgentHistoryValue.copyObject(fields);
        this.source = SourceLocation.from(fields.get("source"));
        this.resume = ResumeLocation.from(fields.get("resume"));
    }

    static LocationResult from(Object value) {
        return new LocationResult(AgentHistoryValue.object(value));
    }

    public String getCtxSessionId() {
        return AgentHistoryValue.string(fields.get("ctxSessionId"));
    }

    public String ctxSessionId() {
        return getCtxSessionId();
    }

    public String getCtxEventId() {
        return AgentHistoryValue.string(fields.get("ctxEventId"));
    }

    public String ctxEventId() {
        return getCtxEventId();
    }

    public String getProvider() {
        return AgentHistoryValue.string(fields.get("provider"));
    }

    public String provider() {
        return getProvider();
    }

    public String getProviderSessionId() {
        return AgentHistoryValue.string(fields.get("providerSessionId"));
    }

    public String providerSessionId() {
        return getProviderSessionId();
    }

    public SourceLocation getSource() {
        return source;
    }

    public SourceLocation source() {
        return source;
    }

    public ResumeLocation getResume() {
        return resume;
    }

    public ResumeLocation resume() {
        return resume;
    }

    public Map<String, Object> asMap() {
        return fields;
    }
}
