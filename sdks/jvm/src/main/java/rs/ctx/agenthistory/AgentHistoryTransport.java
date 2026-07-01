package rs.ctx.agenthistory;

/** Transport for agent-history-v1 operations. */
public interface AgentHistoryTransport {
    String name();

    String execute(AgentHistoryOperation operation);

    default String ctxVersion() {
        return null;
    }
}

