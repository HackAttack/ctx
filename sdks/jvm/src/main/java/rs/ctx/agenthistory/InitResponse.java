package rs.ctx.agenthistory;

import java.util.Map;

/** Response returned by {@link AgentHistoryClient#init(AgentHistoryOptions.Init)}. */
public final class InitResponse extends AgentHistoryEnvelope {
    private final StatusRecord status;

    InitResponse(Map<String, Object> canonical) {
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
