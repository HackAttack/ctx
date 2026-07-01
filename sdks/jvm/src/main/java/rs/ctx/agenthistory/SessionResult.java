package rs.ctx.agenthistory;

import java.util.List;
import java.util.Map;

/** Show-session payload containing transcript metadata and events. */
public final class SessionResult {
    private final Map<String, Object> fields;
    private final SessionSummary session;
    private final List<Event> events;
    private final SourceLocation source;

    SessionResult(Map<String, Object> fields) {
        this.fields = AgentHistoryValue.copyObject(fields);
        this.session = SessionSummary.from(fields.get("session"));
        this.events = AgentHistoryValue.objectList(fields.get("events"), Event::new);
        this.source = SourceLocation.from(fields.get("source"));
    }

    static SessionResult from(Object value) {
        return new SessionResult(AgentHistoryValue.object(value));
    }

    public SessionSummary getSession() {
        return session;
    }

    public SessionSummary session() {
        return session;
    }

    public List<Event> getEvents() {
        return events;
    }

    public List<Event> events() {
        return events;
    }

    public SourceLocation getSource() {
        return source;
    }

    public SourceLocation source() {
        return source;
    }

    public String getMode() {
        return AgentHistoryValue.string(fields.get("mode"));
    }

    public String mode() {
        return getMode();
    }

    public String getFormat() {
        return AgentHistoryValue.string(fields.get("format"));
    }

    public String format() {
        return getFormat();
    }

    public Map<String, Object> asMap() {
        return fields;
    }
}
