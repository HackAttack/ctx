package rs.ctx.agenthistory;

import java.util.Map;

/** Response returned by {@link AgentHistoryClient#status()}. */
public final class StatusResponse extends AgentHistoryEnvelope {
    private final StatusRecord status;

    StatusResponse(Map<String, Object> canonical) {
        super(canonical);
        this.status = StatusRecord.from(payload("status"));
    }

    public StatusRecord getStatus() {
        return status;
    }

    public StatusRecord status() {
        return status;
    }
}
