package rs.ctx.agenthistory;

import java.util.Map;

/** Response returned by {@link AgentHistoryClient#showSession(String, AgentHistoryOptions.ShowSession)}. */
public final class ShowSessionResponse extends AgentHistoryEnvelope {
    private final SessionResult session;

    ShowSessionResponse(Map<String, Object> canonical) {
        super(canonical);
        this.session = SessionResult.from(payload("session"));
    }

    public SessionResult getSession() {
        return session;
    }

    public SessionResult session() {
        return session;
    }
}
