package rs.ctx.agenthistory;

import java.util.Map;

/** Response returned by {@link AgentHistoryClient#showEvent(String, AgentHistoryOptions.ShowEvent)}. */
public final class ShowEventResponse extends AgentHistoryEnvelope {
    private final EventResult event;

    ShowEventResponse(Map<String, Object> canonical) {
        super(canonical);
        this.event = EventResult.from(payload("event"));
    }

    public EventResult getEvent() {
        return event;
    }

    public EventResult event() {
        return event;
    }
}
